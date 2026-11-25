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
        let manifest: Manifest = crate::download::client()?
            .get(MANIFEST)
            .send()?
            .error_for_status()?
            .json()?;
        atomic_write(&index, &serde_json::to_vec(&manifest)?)?;
        manifest
    };
    // Older releases require the legacy assets layout and authentication arguments.
    Ok(manifest
        .versions
        .into_iter()
        .filter(|v| v.released.as_str() >= "2021-06-08" && safe_id(&v.id))
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
        .context("官方版本列表中找不到这个版本（支持 1.17 起的版本）")?;
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
    let child: VersionJson = if pinned.is_file() {
        let child: VersionJson = serde_json::from_slice(&fs::read(&pinned)?)?;
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
        install_only,
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

fn prepare_and_play(
    root: &Path,
    instance: &Instance,
    player: Option<&crate::accounts::Player>,
    install_only: bool,
    cancel: &AtomicBool,
    report: &(dyn Fn(String) + Sync),
    journal: &mut File,
) -> Result<()> {
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
    let version = match &instance.external {
        Some(link) => link.version(),
        None => version(root, &shared, instance, &downloads),
    }
    .context("读取版本信息失败")?;
    let java = java::select(
        &root.join("shared"),
        &version,
        &downloads,
        instance.java_path.as_deref(),
    )?;
    let mut files = vanilla::plan_vanilla_downloads(&version, &shared)?.tasks;
    if instance.external.is_some() && !install_only {
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
    report("解压本地库".into());
    let native_dir = natives::extract_natives(
        &version.libraries,
        &shared,
        &format!("mcstart-{}", instance.id),
    )?;
    if install_only {
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
    let options = LaunchOptions {
        account: crate::accounts::identity(Some(player))?,
        demo: false,
        custom_resolution: instance.resolution,
        game_directory: Some(game_dir),
        natives_directory: Some(native_dir),
        java_executable: Some(java),
        launcher_name: crate::APP_NAME.into(),
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
    let output = File::create(dir.join("last-launch.log"))?;
    let mut command = Command::new(plan.executable);
    java::hide_console(&mut command);
    downloads.check()?;
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
    drop(install_lock);
    report(format!("游戏已启动 · PID {}", child.id()));
    writeln!(journal, "PID {}", child.id())?;
    let exit = child.wait()?;
    writeln!(journal, "{exit}")?;
    if !exit.success() {
        let log = crate::logs::tail(&dir.join("last-launch.log")).unwrap_or_default();
        let reason = crate::logs::diagnosis(&log).unwrap_or("请查看控制台中的游戏日志。");
        anyhow::bail!("游戏异常退出（{exit}）。{reason}");
    }
    Ok(())
}
