use std::{
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
    let mut command = Command::new(java);
    hide_console(&mut command);
    let reply = command.arg("-version").output().ok()?;
    if !reply.status.success() {
        return None;
    }
    let output = String::from_utf8_lossy(&reply.stderr);
    let version = output
        .lines()
        .find_map(|line| line.split_once("version \"").map(|(_, s)| s))?;
    version
        .trim_start_matches("1.")
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

pub fn select(
    shared: &Path,
    version: &VersionJson,
    downloads: &Downloads,
    override_path: Option<&Path>,
) -> Result<PathBuf> {
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
            version_of(&java).with_context(|| format!("无法执行所选 Java：{}", java.display()))?;
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
    if ready.is_file() && version_of(&managed) == Some(major) {
        return Ok(managed);
    }
    let from_env = env::var_os("JAVA_HOME").map(|home| PathBuf::from(home).join("bin/java.exe"));
    for path in [from_env, Some(PathBuf::from("java"))]
        .into_iter()
        .flatten()
    {
        if version_of(&path) == Some(major) {
            return Ok(path);
        }
    }
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
