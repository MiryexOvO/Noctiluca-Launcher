use std::{
    collections::HashSet,
    env, fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use mc_launcher_core::{
    core::version::VersionJson,
    net::download::{Checksum, DownloadTask},
    types::runtime_types::{PlatformManifestJson, RuntimeListJson},
};

use crate::{download::Downloads, library::safe_id};

const RUNTIMES: &str = "https://piston-meta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";

pub fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

fn version_of(java: &Path) -> Option<u32> {
    // javaw is the GUI launcher. Its console sibling reports the same VM version.
    let console = java.with_file_name("java.exe");
    let probe = if java.file_name()?.eq_ignore_ascii_case("javaw.exe") && console.is_file() {
        console.as_path()
    } else {
        java
    };
    let mut command = Command::new(probe);
    hide_console(&mut command);
    let reply = command.arg("-version").output().ok()?;
    if !reply.status.success() {
        return None;
    }
    [reply.stderr, reply.stdout].iter().find_map(|bytes| {
        let output = String::from_utf8_lossy(bytes);
        let version = output
            .lines()
            .find_map(|line| line.split_once("version \"").map(|(_, s)| s))?;
        version
            .trim_start_matches("1.")
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    })
}

// Only cache real runtimes. PATH shims may change their target without changing the shim.
type FileStamp = (u64, u64, u64);
type RuntimeStamp = (FileStamp, FileStamp, Option<FileStamp>, Option<FileStamp>);
fn probe_major(shared: &Path, java: &Path, quick: bool) -> Option<u32> {
    use sha2::{Digest, Sha256};
    use std::os::windows::fs::MetadataExt;
    let identify = || {
        let bin = dunce::canonicalize(java).ok()?;
        let home = bin.parent()?.parent()?;
        let vm = [
            "bin/server/jvm.dll",
            "jre/bin/server/jvm.dll",
            "bin/client/jvm.dll",
        ]
        .into_iter()
        .map(|n| home.join(n))
        .find(|p| p.is_file())?;
        let stamp = |path: &Path| {
            let m = fs::metadata(path).ok()?;
            Some((m.len(), m.last_write_time(), m.creation_time()))
        };
        let identity = (
            stamp(&bin)?,
            stamp(&vm)?,
            stamp(&home.join("release")),
            stamp(&bin.with_file_name("java.exe")),
        );
        let key = hex::encode(Sha256::digest(bin.to_str()?.as_bytes()));
        Some((
            shared.join("java-probes").join(format!("{key}.json")),
            identity,
        ))
    };
    let Some((record, identity)) = identify() else {
        return version_of(java);
    };
    if quick
        && let Ok(bytes) = fs::read(&record)
        && let Ok((stored, major)) = serde_json::from_slice::<(RuntimeStamp, u32)>(&bytes)
        && stored == identity
    {
        return Some(major);
    }
    let major = version_of(java)?;
    if let Ok(bytes) = serde_json::to_vec(&(identity, major)) {
        let _ = crate::library::atomic_write(&record, &bytes);
    }
    Some(major)
}

fn child_directories(path: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
}

fn installed_candidates(shared: &Path, platform: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for key in ["JAVA_HOME", "JDK_HOME"] {
        if let Some(home) = env::var_os(key) {
            paths.push(PathBuf::from(home).join("bin/java.exe"));
        }
    }
    if let Some(path) = env::var_os("PATH") {
        paths.extend(
            env::split_paths(&path)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join("java.exe")),
        );
    }
    for home in child_directories(&shared.join("runtime").join(platform)) {
        if home.join("installed").is_file() {
            paths.push(home.join("bin/java.exe"));
        }
    }
    let mut roots = Vec::new();
    for key in ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"] {
        if let Some(base) = env::var_os(key) {
            let mut base = PathBuf::from(base);
            if key == "LOCALAPPDATA" {
                base.push("Programs");
            }
            for vendor in [
                "Java",
                "Eclipse Adoptium",
                "Microsoft",
                "Amazon Corretto",
                "BellSoft",
                "Zulu",
                "Semeru",
            ] {
                roots.push(base.join(vendor));
            }
        }
    }
    if let Some(profile) = env::var_os("USERPROFILE") {
        roots.push(PathBuf::from(profile).join(".jdks"));
    }
    roots.sort();
    roots.dedup();
    for root in roots {
        paths.extend(child_directories(&root).map(|dir| dir.join("bin/java.exe")));
    }
    if let Some(appdata) = env::var_os("APPDATA") {
        for component in child_directories(&PathBuf::from(appdata).join(".minecraft/runtime")) {
            if let Some(name) = component.file_name() {
                paths.push(component.join(platform).join(name).join("bin/java.exe"));
            }
        }
    }
    paths
}

fn choose_installed(
    candidates: impl IntoIterator<Item = PathBuf>,
    required: u32,
    mut probe: impl FnMut(&Path) -> Option<u32>,
) -> Option<(u32, PathBuf)> {
    let mut best = None;
    let mut seen = HashSet::new();
    for path in candidates {
        if !path.is_absolute() || !path.is_file() {
            continue;
        }
        let identity = dunce::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !seen.insert(identity) {
            continue;
        }
        let Some(major) = probe(&path) else { continue };
        if major == required {
            return Some((major, path));
        }
        if required != 8 && major > required && best.as_ref().is_none_or(|(v, _)| major < *v) {
            best = Some((major, path));
        }
    }
    best
}

pub fn select(
    shared: &Path,
    version: &VersionJson,
    downloads: &Downloads,
    override_path: Option<&Path>,
) -> Result<PathBuf> {
    select_mode(shared, version, downloads, override_path, false)
}

pub fn select_mode(
    shared: &Path,
    version: &VersionJson,
    downloads: &Downloads,
    override_path: Option<&Path>,
    quick: bool,
) -> Result<PathBuf> {
    let probe = |p: &Path| probe_major(shared, p, quick);
    let spec = version
        .java_version
        .as_ref()
        .context("版本缺少 Java 要求")?;
    let major = u32::try_from(spec.major_version)?;
    (downloads.report)(format!("检查 Java {major}"));
    if let Some(java) = override_path
        .map(Path::to_owned)
        .or_else(|| crate::setting("JAVA").map(PathBuf::from))
    {
        let detected =
            probe(&java).with_context(|| format!("无法执行所选 Java：{}", java.display()))?;
        ensure!(
            major != 8 || detected == 8,
            "此旧版游戏需要 Java 8，所选版本为 {detected}"
        );
        ensure!(
            detected >= major,
            "此版本至少需要 Java {major}，所选版本为 {detected}"
        );
        return Ok(java);
    }
    let component = &spec.component;
    ensure!(safe_id(component), "Java 组件名称无效");
    let platform = match (env::consts::OS, env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x64",
        ("windows", "aarch64") => "windows-arm64",
        _ => bail!("当前系统请用 NOCTILUCA_JAVA 指定 Java {major}"),
    };
    let dir = shared.join("runtime").join(platform).join(component);
    let managed = dir.join("bin/java.exe");
    let ready = dir.join("installed");
    if ready.is_file() && probe(&managed) == Some(major) {
        (downloads.report)(format!("使用已有 Java {major}"));
        return Ok(managed);
    }
    if let Some((found, path)) =
        choose_installed(installed_candidates(shared, platform), major, probe)
    {
        (downloads.report)(format!("使用本机 Java {found} · {}", path.display()));
        return Ok(path);
    }
    downloads.check()?;
    (downloads.report)(format!("正在获取 Mojang Java {major}"));
    let all: RuntimeListJson = downloads
        .http
        .get(RUNTIMES)
        .send()?
        .error_for_status()?
        .json()?;
    let bundle = all
        .get(platform)
        .and_then(|r| r.get(component))
        .and_then(|r| r.first())
        .with_context(|| format!("此平台没有可下载的 Java {major}，请设置 NOCTILUCA_JAVA"))?;
    let manifest = dir.join("runtime.json");
    downloads.batch(
        &dir,
        "Java 安装清单",
        vec![DownloadTask {
            url: bundle.manifest.url.clone(),
            destination: manifest.clone(),
            checksum: Some(Checksum::Sha1(bundle.manifest.sha1.clone())),
            label: component.clone(),
        }],
    )?;
    let files: PlatformManifestJson = serde_json::from_slice(&fs::read(manifest)?)?;
    let mut tasks = Vec::new();
    for (name, file) in files.files {
        let relative = Path::new(&name);
        ensure!(
            relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
            "Java 文件路径无效"
        );
        match file.r#type.as_deref() {
            Some("directory") => fs::create_dir_all(dir.join(relative))?,
            Some("file") => {
                let raw = file
                    .downloads
                    .and_then(|mut d| d.remove("raw"))
                    .context("Java 清单缺少原始文件")?;
                tasks.push(DownloadTask {
                    url: raw.url,
                    destination: dir.join(relative),
                    checksum: Some(Checksum::Sha1(raw.sha1)),
                    label: name,
                });
            }
            _ => bail!("此 Java 包包含不支持的文件类型：{name}"),
        }
    }
    downloads.batch(&dir, &format!("安装 Java {major}"), tasks)?;
    ensure!(version_of(&managed) == Some(major), "下载的 Java 自检失败");
    crate::library::atomic_write(&ready, component.as_bytes())?;
    Ok(managed)
}
