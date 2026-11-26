use std::{
    fs::{self, File},
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::atomic::AtomicBool,
};

use anyhow::{Context, Result, ensure};
use mc_launcher_core::{
    core::version::VersionJson,
    install::{assets, natives, vanilla},
    loader::{fabric, quilt},
    net::download::{Checksum, DownloadTask},
    prelude::*,
};
use serde::{Deserialize, Serialize};

use crate::{
    download::Downloads,
    java,
    library::{Instance, Loader, atomic_write, safe_id},
};

const MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Clone, Deserialize, Serialize)]
pub struct Entry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    url: String,
    sha1: String,
    #[serde(rename = "releaseTime")]
    released: String,
}
#[derive(Serialize, Deserialize)]
struct Manifest {
    versions: Vec<Entry>,
}

pub fn catalog(root: &Path, refresh: bool) -> Result<Vec<Entry>> {
    let index = root.join("versions.json");
    let manifest: Manifest = if !refresh && index.is_file() {
        serde_json::from_slice(&fs::read(index)?)?
    } else {
        let http = crate::download::client()?;
        let mut result = None;
        for url in crate::preferences::sources(MANIFEST, crate::preferences::current().catalog) {
            result = Some(
                http.get(url)
                    .send()
                    .and_then(|r| r.error_for_status())
                    .and_then(|r| r.json::<Manifest>()),
            );
            if result.as_ref().is_some_and(|r| r.is_ok()) {
                break;
            }
        }
        let manifest = result.context("没有可用版本源")??;
        atomic_write(&index, &serde_json::to_vec(&manifest)?)?;
        manifest
    };
    // These two older releases use Java 8 and the hashed asset-object layout.
    Ok(manifest
        .versions
        .into_iter()
        .filter(|v| {
            safe_id(&v.id)
                && (v.released.as_str() >= "2021-06-08"
                    || matches!(v.id.as_str(), "1.12.2" | "1.16.5"))
        })
        .collect())
}

fn version(
    root: &Path,
    shared: &Path,
    instance: &Instance,
    downloads: &Downloads,
) -> Result<VersionJson> {
    let base = &instance.version;
    let base_file = shared
        .join("versions")
        .join(base)
        .join(format!("{base}.json"));
    let mut entries = catalog(root, false)?;
    if !entries.iter().any(|v| &v.id == base) {
        entries = catalog(root, true)?;
    }
    let entry = entries
        .iter()
        .find(|v| &v.id == base)
        .context("官方版本列表中找不到这个版本（支持 1.12.2、1.16.5 和 1.17 起的版本）")?;
    downloads.batch(
        shared,
        "版本文件",
        vec![DownloadTask {
            url: entry.url.clone(),
            destination: base_file.clone(),
            checksum: Some(Checksum::Sha1(entry.sha1.clone())),
            label: base.clone(),
        }],
    )?;
    let vanilla: VersionJson = serde_json::from_slice(&fs::read(base_file)?)?;
    ensure!(vanilla.id.as_ref() == Some(base), "版本元数据不匹配");
    if instance.loader == Loader::Vanilla {
        return Ok(vanilla);
    }
    let pinned = instance.path(root).join("loader.json");
    let cached = if pinned.is_file() {
        serde_json::from_slice::<VersionJson>(&fs::read(&pinned)?).ok()
    } else {
        None
    };
    let child = if let Some(child) = cached.filter(|c| {
        if c.inherits_from.as_ref() != Some(base) {
            return false;
        }
        let id = c.id.as_deref().unwrap_or("");
        let build = match instance.loader {
            Loader::Fabric => id
                .strip_prefix("fabric-loader-")
                .and_then(|s| s.strip_suffix(&format!("-{base}"))),
            Loader::Quilt => id
                .strip_prefix("quilt-loader-")
                .and_then(|s| s.strip_suffix(&format!("-{base}"))),
            Loader::NeoForge => id.strip_prefix("neoforge-"),
            Loader::Forge => id.strip_prefix(&format!("{base}-forge-")),
            Loader::Vanilla => None,
        };
        build.is_some_and(|v| {
            instance
                .loader_version
                .as_deref()
                .is_none_or(|wanted| v == wanted)
        })
    }) {
        if matches!(instance.loader, Loader::Forge | Loader::NeoForge)
            && crate::forge::missing_processor_output(&child, shared)
        {
            crate::forge::install(
                root,
                shared,
                instance,
                &vanilla,
                downloads,
                child.id.as_deref(),
            )?
        } else {
            child
        }
    } else {
        (downloads.report)(format!("正在解析 {} 加载器", instance.loader.label()));
        let profile = match instance.loader {
            Loader::Fabric => {
                let v = match &instance.loader_version {
                    Some(v) => v.clone(),
                    None => fabric::latest_stable_loader(&fabric::list_loader_versions()?)?
                        .version
                        .clone(),
                };
                fabric::fetch_profile(base, &v)?
            }
            Loader::Quilt => {
                let v = match &instance.loader_version {
                    Some(v) => v.clone(),
                    None => quilt::latest_loader(&quilt::list_loader_versions()?)?
                        .version
                        .clone(),
                };
                quilt::fetch_profile(base, &v)?
            }
            Loader::Vanilla => unreachable!(),
            Loader::Forge | Loader::NeoForge => {
                crate::forge::install(root, shared, instance, &vanilla, downloads, None)?
            }
        };
        ensure!(
            profile.inherits_from.as_ref() == Some(base),
            "加载器与游戏版本不匹配"
        );
        atomic_write(&pinned, &serde_json::to_vec_pretty(&profile)?)?;
        profile
    };
    ensure!(
        child.inherits_from.as_ref() == Some(base) && child.id.as_deref().is_some_and(safe_id),
        "加载器配置无效"
    );
    Ok(vanilla.merge_child(&child))
}

pub fn play(
    root: &Path,
    instance: &Instance,
    player: Option<&crate::accounts::Player>,
    install_only: bool,
    cancel: &AtomicBool,
    report: impl Fn(String) + Sync,
) -> Result<()> {
    let dir = instance.path(root);
    let instance_lock =
        crate::library::lock_instance(root, instance, "这个实例正在另一窗口中使用")?;
    let mut journal = File::create(dir.join("launcher.log"))?;
    let result = prepare_and_play(
        root,
        instance,
        player,
        if install_only {
            RunMode::Verify
        } else {
            RunMode::Launch
        },
        cancel,
        &report,
        &mut journal,
    );
    if let Err(error) = &result {
        writeln!(journal, "{error:#}")?;
    }
    drop(instance_lock);
    result
}

pub(crate) fn lock_file(path: &Path, message: &str) -> Result<File> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.try_lock().context(message.to_owned())?;
    Ok(file)
}

#[derive(PartialEq, Eq)]
enum RunMode {
    Stage,
    Verify,
    Launch,
}
fn prepare_and_play(
    root: &Path,
    instance: &Instance,
    player: Option<&crate::accounts::Player>,
    mode: RunMode,
    cancel: &AtomicBool,
    report: &(dyn Fn(String) + Sync),
    journal: &mut File,
) -> Result<()> {
    let prefs = crate::preferences::current();
    let game_dir = instance.game_dir(root)?;
    let shared = instance
        .external
        .as_ref()
        .map_or_else(|| root.join("shared"), |link| link.origin.folder.clone());
    fs::create_dir_all(&shared)?;
    let install_lock = lock_file(
        &shared.join(if instance.external.is_some() {
            ".noctiluca-install.lock"
        } else {
            "install.lock"
        }),
        "另一个窗口正在安装游戏，请稍后重试",
    )?;
    let mut downloads = Downloads::new(cancel, report)?;
    downloads.cache_source = instance
        .origin
        .as_ref()
        .map(|o| o.folder.clone())
        .or(crate::library::cache_source(root)?);
    let dir = instance.path(root);

    fs::create_dir_all(game_dir.join("mods"))?;
    report("读取游戏版本信息".into());
    writeln!(
        journal,
        "{} · {} · {}",
        instance.name,
        instance.version,
        instance.loader.label()
    )?;
    let mut version = match &instance.external {
        Some(link) => link.version(),
        None => version(root, &shared, instance, &downloads),
    }
    .context("读取版本信息失败")?;
    if prefs.authlib && instance.external.is_none() {
        upgrade_cached_authlib(root, &shared, &mut version)?;
    }
    let java = java::select(
        &root.join("shared"),
        &version,
        &downloads,
        instance.java_path.as_deref().or(prefs.java.as_deref()),
    )?;
    let mut files = vanilla::plan_vanilla_downloads(&version, &shared)?.tasks;
    if instance.external.is_some() && mode == RunMode::Launch {
        // Existing launchers may use a locally patched client or libraries.
        files.retain(|task| !task.destination.is_file());
    }
    downloads.batch(&shared, "游戏与运行库", files)?;
    if let Some(index) = &version.asset_index {
        let path = assets::asset_index_path(&shared, &index.id);
        let objects = serde_json::from_slice(&fs::read(path)?)?;
        downloads.batch(
            &shared,
            "声音与游戏资源",
            assets::plan_asset_object_downloads_from_index(&objects, &shared).tasks,
        )?;
    }
    downloads.check()?;
    if mode == RunMode::Stage {
        return Ok(());
    }
    report("解压本地库".into());
    let native_dir = natives::extract_natives(
        &version.libraries,
        &shared,
        &format!("mcstart-{}", instance.id),
    )?;
    if mode == RunMode::Verify {
        writeln!(journal, "游戏文件校验完成")?;
        return Ok(());
    }
    writeln!(journal, "Java: {}", java.display())?;
    let player = player.context("请先添加并选择游戏账号")?;
    report(format!("准备{}账号", player.kind.label()));
    let agent = if player.kind == crate::accounts::Kind::ThirdParty {
        Some(crate::third_party::agent(&shared, player, &downloads)?)
    } else {
        None
    };
    let (initial, max) = crate::memory::effective(
        instance,
        crate::memory::Global::read(root)?,
        crate::memory::physical().as_ref(),
    );
    crate::preferences::set_language(&game_dir, &prefs)?;
    let options = LaunchOptions {
        account: crate::accounts::identity(Some(player))?,
        demo: false,
        custom_resolution: instance.resolution.or(prefs.resolution()),
        game_directory: Some(game_dir),
        natives_directory: Some(native_dir),
        java_executable: Some(java),
        launcher_name: prefs.brand.clone(),
        launcher_version: env!("CARGO_PKG_VERSION").into(),
        ..Default::default()
    };
    let launcher = Launcher::new(shared);
    let mut plan = launcher.build_launch_command_from_version(&version, options)?;
    if player.kind != crate::accounts::Kind::Microsoft
        && let Some(pos) = plan.args.iter().position(|s| s == "--userType")
        && let Some(value) = plan.args.get_mut(pos + 1)
    {
        *value = if player.kind == crate::accounts::Kind::Offline {
            "legacy"
        } else {
            "mojang"
        }
        .into();
    }
    if prefs.join_server {
        let (server, port) = crate::preferences::server(&prefs.server)?;
        let modern =
            version.arguments.game.iter().any(|a| {
                serde_json::to_string(a).is_ok_and(|s| s.contains("quickPlayMultiplayer"))
            });
        if modern {
            plan.args
                .extend(["--quickPlayMultiplayer".into(), format!("{server}:{port}")]);
        } else {
            plan.args
                .extend(["--server".into(), server, "--port".into(), port.to_string()]);
        }
    }
    let output = File::create(dir.join("last-launch.log"))?;
    let mut command = Command::new(plan.executable);
    use std::os::windows::process::CommandExt;
    command.creation_flags(
        0x08000000
            | match prefs.priority {
                0 => 0x4000,
                2 => 0x8000,
                _ => 0x20,
            },
    );
    match prefs.ip {
        1 => {
            command.arg("-Djava.net.preferIPv4Stack=true");
        }
        2 => {
            command.arg("-Djava.net.preferIPv6Addresses=true");
        }
        _ => {}
    }

    downloads.check()?;
    let launched_at = std::time::SystemTime::now();
    let mut child = command
        .arg(format!("-Xms{initial}M"))
        .arg(format!("-Xmx{max}M"))
        .args(agent)
        .args(
            instance
                .jvm_args
                .iter()
                .filter(|arg| !arg.starts_with("-Xmx") && !arg.starts_with("-Xms")),
        )
        .args(plan.args)
        .envs(plan.env)
        .current_dir(plan.working_dir)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .spawn()
        .context("无法启动 Java 进程")?;
    if prefs.game_title != 0 {
        crate::preferences::set_window_title(
            child.id(),
            if prefs.game_title == 1 {
                instance.name.clone()
            } else {
                prefs.brand.clone()
            },
        );
    }
    drop(install_lock);
    if let Err(e) = crate::organization::Organization::edit(root, &instance.id, |v| {
        v.played = chrono::Utc::now().timestamp_millis()
    }) {
        let _ = writeln!(journal, "最近游玩记录：{e}");
    }
    report(format!("游戏已启动 · PID {}", child.id()));
    writeln!(journal, "PID {}", child.id())?;
    let exit = child.wait()?;
    writeln!(journal, "{exit}")?;
    if !exit.success() {
        let reason = crate::logs::launch_failure(root, instance, launched_at);
        return Err(anyhow::Error::new(reason).context(format!("游戏异常退出（{exit}）")));
    }
    Ok(())
}

fn upgrade_cached_authlib(root: &Path, shared: &Path, version: &mut VersionJson) -> Result<()> {
    fn revision(name: &str) -> Option<(u32, u32, u32)> {
        let n = name.strip_prefix("com.mojang:authlib:")?;
        let mut nums = n.split('.');
        let r = (
            nums.next()?.parse().ok()?,
            nums.next()?.parse().ok()?,
            nums.next()?.parse().ok()?,
        );
        nums.next().is_none().then_some(r)
    }
    let Some(lib) = version
        .libraries
        .iter_mut()
        .find(|l| revision(&l.name).is_some())
    else {
        return Ok(());
    };
    let (major, minor, mut patch) = revision(&lib.name).unwrap();
    let entries = if let Ok(b) = fs::read(root.join("versions.json")) {
        serde_json::from_slice::<Manifest>(&b)?.versions
    } else {
        return Ok(());
    };
    for entry in entries
        .into_iter()
        .filter(|e| e.kind == "release" && safe_id(&e.id))
    {
        let path = shared
            .join("versions")
            .join(&entry.id)
            .join(format!("{}.json", entry.id));
        if !path.is_file()
            || !mc_launcher_core::io::hash::sha1_file(&path)?.eq_ignore_ascii_case(&entry.sha1)
        {
            continue;
        }
        let cached: VersionJson = serde_json::from_slice(&fs::read(path)?)?;
        for candidate in cached.libraries {
            if let Some((a, b, c)) = revision(&candidate.name)
                && a == major
                && b == minor
                && c > patch
                && candidate
                    .downloads
                    .as_ref()
                    .and_then(|d| d.artifact.as_ref())
                    .is_some_and(|d| {
                        d.url.starts_with("https://libraries.minecraft.net/") && d.sha1.len() == 40
                    })
            {
                *lib = candidate;
                patch = c;
            }
        }
    }
    Ok(())
}

pub fn stage_loader(
    root: &Path,
    item: &Instance,
    cancel: &AtomicBool,
    report: &(dyn Fn(String) + Sync),
) -> Result<Option<Vec<u8>>> {
    let temp = tempfile::Builder::new()
        .prefix("prepare-")
        .tempdir_in(root.join("instances"))?;
    let mut staged = item.clone();
    staged.id = temp
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    staged.external = None;
    let mut journal = File::create(temp.path().join("launcher.log"))?;
    prepare_and_play(
        root,
        &staged,
        None,
        RunMode::Stage,
        cancel,
        report,
        &mut journal,
    )?;
    if staged.loader == Loader::Vanilla {
        Ok(None)
    } else {
        Ok(Some(fs::read(temp.path().join("loader.json"))?))
    }
}
