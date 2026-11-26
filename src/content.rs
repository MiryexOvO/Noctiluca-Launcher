pub(crate) mod review;
use crate::{
    backups,
    library::{self, Instance, Loader},
    packs,
};
use anyhow::{Context, Result, bail, ensure};
use mc_launcher_core::io::hash::sha1_file;
use review::{Choices, Review};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

pub fn resource_dir(root: &Path, item: &Instance, kind: &str) -> Result<PathBuf> {
    ensure!(
        matches!(
            kind,
            "saves" | "mods" | "resourcepacks" | "shaderpacks" | "screenshots" | "schematics"
        ),
        "未知资源目录"
    );
    let game = item.game_dir(root)?;
    if game.join(".noctiluca-mod-update").exists() {
        let _recovered = library::lock_instance(root, item, "正在更新实例模组")?;
    }
    let path = game.join(kind);
    fs::create_dir_all(&path).with_context(|| format!("无法创建 {}", path.display()))?;
    dunce::canonicalize(&path).with_context(|| format!("找不到 {}", path.display()))
}

pub fn explore(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    ensure!(path.is_dir(), "目录不存在：{}", path.display());
    let path = dunce::canonicalize(path)?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    ensure!(
        (result as isize) > 32,
        "Windows 无法打开 {}（Shell 错误 {}）",
        path.display(),
        result as isize
    );
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Upstream {
    pub source: crate::market::Source,
    pub project: String,
    pub release: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
}
impl From<&crate::market::Release> for Upstream {
    fn from(r: &crate::market::Release) -> Self {
        Self {
            source: r.source,
            project: r.project.clone(),
            release: r.id.clone(),
            icon_url: None,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct PackState {
    pub upstream: Option<Upstream>,
    pub title: String,
    #[serde(default)]
    pub archive_sha1: Option<String>,
    pub(crate) files: BTreeMap<String, String>,
}
fn visit(
    dir: &Path,
    base: &Path,
    callback: &mut impl FnMut(&Path, &str) -> Result<()>,
) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    for e in fs::read_dir(dir)? {
        let path = e?.path();
        let meta = fs::symlink_metadata(&path)?;
        ensure!(
            meta.file_attributes() & 0x400 == 0,
            "不支持链接：{}",
            path.display()
        );
        if meta.is_dir() {
            visit(&path, base, callback)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(base)?
                .to_str()
                .context("文件名编码无效")?
                .replace('\\', "/");
            packs::relative(&rel)?;
            callback(&path, &rel)?;
        }
    }
    Ok(())
}
impl PackState {
    pub fn from_game(dir: &Path, upstream: Option<Upstream>, title: &str) -> Result<Self> {
        let mut files = BTreeMap::new();
        visit(dir, dir, &mut |p, k| {
            files.insert(k.to_owned(), sha1_file(p)?);
            Ok(())
        })?;
        Ok(Self {
            upstream,
            title: title.into(),
            archive_sha1: None,
            files,
        })
    }
    pub fn read(dir: &Path) -> Result<Option<Self>> {
        match fs::read(dir.join("pack-state.json")) {
            Ok(v) => Ok(Some(serde_json::from_slice(&v)?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
fn protected(rel: &str) -> bool {
    let head = rel.split('/').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        head.as_str(),
        "saves"
            | "screenshots"
            | "servers.dat"
            | "options.txt"
            | "journeymap"
            | "xaero"
            | "xaerowaypoints"
            | "xaeroworldmap"
    )
}
#[derive(Serialize, Deserialize)]
struct Swap {
    instance: String,
    backup: String,
}
pub fn recover(root: &Path) -> Result<()> {
    let journal = root.join("pack-swap.json");
    if !journal.try_exists()? {
        return Ok(());
    }
    let swap: Swap = serde_json::from_slice(&fs::read(&journal)?)?;
    ensure!(
        library::safe_id(&swap.instance) && library::safe_id(&swap.backup),
        "更新恢复记录无效"
    );
    let live = root.join("instances").join(&swap.instance);
    let backup = root.join("backups").join(&swap.instance).join(swap.backup);
    if !live.try_exists()? {
        fs::rename(&backup, &live).context("恢复中断的实例更新失败")?;
    } else if backup.is_dir() {
        library::atomic_write(&backup.join("before-pack-update"), b"")?;
    }
    fs::remove_file(journal)?;
    Ok(())
}

pub(crate) struct Merge<'a> {
    pub review: Option<&'a Review>,
    pub choices: &'a Choices,
    pub task: Option<&'a str>,
}

pub fn apply_pack(
    root: &Path,
    item: &Instance,
    new: &Instance,
    source: &Path,
    state: &PackState,
    merge: Merge<'_>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Instance> {
    use std::sync::atomic::Ordering;
    ensure!(item.external.is_none(), "原地更新仅支持启动器管理的实例");
    let lock = library::lock_instance(root, item, "请先退出正在使用此实例的游戏")?;
    let live = item.path(root);
    let current: Instance = serde_json::from_slice(&fs::read(live.join("instance.json"))?)?;
    let old = PackState::read(&live)?;
    if let (Some(a), Some(b)) = (
        old.as_ref().and_then(|s| s.upstream.as_ref()),
        state.upstream.as_ref(),
    ) {
        ensure!(
            a.source == b.source && a.project == b.project,
            "所选整合包来自不同项目"
        );
    }
    let tmp = tempfile::tempdir_in(root)?;
    for entry in fs::read_dir(&live)? {
        let e = entry?;
        if e.file_name() != "game.lock" {
            backups::copy_tree(&e.path(), &tmp.path().join(e.file_name()))?;
        }
    }
    let target = tmp.path().join("game");
    fs::create_dir_all(&target)?;
    let fresh = Review::build(&target, old.as_ref(), &state.files)?;
    ensure!(
        merge.review.is_none_or(|seen| seen == &fresh)
            && merge
                .choices
                .iter()
                .all(|(p, c)| fresh.conflicts.get(p) == Some(c)),
        "本地配置或整合包已经变化，请重新检查更新清单"
    );
    let marker = tmp.path().join("pack-task-id");
    if let Some(id) = merge.task {
        fs::write(marker, id)?;
    } else if marker.try_exists()? {
        fs::remove_file(marker)?;
    }
    let incoming = review::indexed(&state.files);
    let previous = old
        .as_ref()
        .map(|s| review::indexed(&s.files))
        .unwrap_or_default();
    if let Some(prev) = &old {
        for (rel, hash) in &prev.files {
            let p = target.join(packs::relative(rel)?);
            if !incoming.contains_key(&rel.to_lowercase())
                && !protected(rel)
                && p.is_file()
                && (merge.choices.contains_key(rel) || sha1_file(&p)? == *hash)
            {
                fs::remove_file(p)?;
            }
        }
    }
    for (rel, hash) in &state.files {
        ensure!(!cancel.load(Ordering::Relaxed), "已取消更新");
        let relative = packs::relative(rel)?;
        let p = target.join(&relative);
        let source = source.join("game").join(&relative);
        if !source.is_file() {
            continue;
        }
        ensure!(sha1_file(&source)? == *hash, "更新来源文件校验失败：{rel}");
        if fresh.conflicts.contains_key(rel) && !merge.choices.contains_key(rel) {
            continue;
        }
        if p.is_file() {
            let actual = sha1_file(&p)?;
            if actual == *hash || protected(rel) {
                continue;
            }
            if !merge.choices.contains_key(rel)
                && previous.get(&rel.to_lowercase()).map(|(_, hash)| *hash) != Some(&actual)
            {
                if rel.starts_with("config/") || rel.starts_with("defaultconfigs/") {
                    continue;
                }
                bail!("本地文件与新版冲突，已保留原实例：{rel}");
            }
        }
        fs::create_dir_all(p.parent().context("缺少上级目录")?)?;
        fs::copy(source, p)?;
    }
    let mut updated = current;
    updated.loader = new.loader;
    updated.version = new.version.clone();
    updated.loader_version = new.loader_version.clone();
    let icon = tmp.path().join(crate::instance_icon::FILENAME);
    let source_icon = source.join(crate::instance_icon::FILENAME);
    if source_icon.is_file() {
        fs::copy(source_icon, &icon)?;
    } else if icon.try_exists()? {
        fs::remove_file(icon)?;
    }
    let pin = tmp.path().join("loader.json");
    if source.join("loader.json").is_file() {
        fs::copy(source.join("loader.json"), &pin)?;
    } else if pin.try_exists()? {
        fs::remove_file(pin)?;
    }
    library::atomic_write(
        &tmp.path().join("instance.json"),
        &serde_json::to_vec_pretty(&updated)?,
    )?;
    library::atomic_write(
        &tmp.path().join("pack-state.json"),
        &serde_json::to_vec(state)?,
    )?;
    ensure!(!cancel.load(Ordering::Relaxed), "已取消更新");
    crate::mods::history::pack_update(&live, tmp.path())?;
    let _library = crate::game::lock_file(&root.join("library.lock"), "实例库正在使用")?;
    recover(root)?;
    if let Some(seen) = merge.review {
        let current_state = PackState::read(&live)?;
        ensure!(
            Review::build(&live.join("game"), current_state.as_ref(), &state.files)? == *seen,
            "更新期间本地配置已更改，已保留原实例，请重新检查更新清单"
        );
    }
    drop(lock);
    let latest: Instance = serde_json::from_slice(&fs::read(live.join("instance.json"))?)?;
    updated = Instance {
        loader: updated.loader,
        version: updated.version,
        loader_version: updated.loader_version,
        ..latest
    };
    library::atomic_write(
        &tmp.path().join("instance.json"),
        &serde_json::to_vec_pretty(&updated)?,
    )?;
    let token = uuid::Uuid::new_v4().to_string();
    let backup = root.join("backups").join(&item.id).join(&token);
    fs::create_dir_all(backup.parent().unwrap())?;
    library::atomic_write(
        &root.join("pack-swap.json"),
        &serde_json::to_vec(&Swap {
            instance: item.id.clone(),
            backup: token,
        })?,
    )?;
    fs::rename(&live, &backup)?;
    if let Err(e) = fs::rename(tmp.path(), &live) {
        recover(root)?;
        return Err(e.into());
    }
    recover(root)?;
    Ok(updated)
}
pub fn rollback(root: &Path, item: &Instance, backup: &Path) -> Result<Instance> {
    let before: Instance = serde_json::from_slice(&fs::read(backup.join("instance.json"))?)?;
    ensure!(before.id == item.id, "备份与实例不匹配");
    let mut state = PackState::read(backup)?.unwrap_or(PackState {
        upstream: None,
        title: before.name.clone(),
        archive_sha1: None,
        files: BTreeMap::new(),
    });
    for (rel, hash) in &mut state.files {
        let p = backup.join("game").join(packs::relative(rel)?);
        if p.is_file() {
            *hash = sha1_file(&p)?;
        }
    }
    apply_pack(
        root,
        item,
        &before,
        backup,
        &state,
        Merge {
            review: None,
            choices: &Choices::new(),
            task: None,
        },
        &false.into(),
    )
}
pub fn pinned_version(root: &Path, item: &Instance) -> Result<String> {
    if let Some(v) = &item.loader_version {
        return Ok(v.clone());
    }
    let profile: serde_json::Value = serde_json::from_slice(
        &fs::read(item.path(root).join("loader.json")).context("请先安装加载器")?,
    )?;
    let id = profile["id"].as_str().context("加载器配置缺少 ID")?;
    let v = match item.loader {
        Loader::Forge => id.split_once("-forge-").map(|(_, v)| v),
        Loader::NeoForge => id.strip_prefix("neoforge-"),
        Loader::Fabric | Loader::Quilt => {
            id.strip_suffix(&format!("-{}", item.version))
                .and_then(|s| {
                    s.strip_prefix("fabric-loader-")
                        .or_else(|| s.strip_prefix("quilt-loader-"))
                })
        }
        _ => None,
    }
    .context("无法读取加载器版本")?;
    ensure!(library::safe_id(v), "无效加载器版本");
    Ok(v.into())
}

pub const EXPORT_GROUPS: [(&str, &str); 4] = [
    ("mods", "模组"),
    ("config", "配置"),
    ("resourcepacks", "资源包"),
    ("shaderpacks", "光影"),
];
pub fn export(root: &Path, item: &Instance, dest: &Path, groups: [bool; 4]) -> Result<()> {
    use std::io::Write;
    let _lock = library::lock_instance(root, item, "请先退出游戏再导出")?;
    let game = item.game_dir(root)?;
    let dir = dest.parent().context("缺少目标路径")?;
    let dir = dunce::canonicalize(dir)?;
    ensure!(
        !dir.starts_with(dunce::canonicalize(&game)?),
        "请选择游戏目录以外的位置"
    );
    let mut out = tempfile::NamedTempFile::new_in(dir)?;
    let mut zip = zip::ZipWriter::new(out.as_file_mut());
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut deps = serde_json::json!({"minecraft":item.version});
    if item.loader != Loader::Vanilla {
        let key = match item.loader {
            Loader::Fabric => "fabric-loader",
            Loader::Quilt => "quilt-loader",
            Loader::Forge => "forge",
            Loader::NeoForge => "neoforge",
            _ => unreachable!(),
        };
        deps[key] = pinned_version(root, item)?.into();
    }
    zip.start_file("modrinth.index.json", opts)?;
    serde_json::to_writer(
        &mut zip,
        &serde_json::json!({"formatVersion":1,"game":"minecraft","versionId":"1.0.0","name":item.name,"files":[],"dependencies":deps}),
    )?;
    for (idx, (part, _)) in EXPORT_GROUPS.iter().enumerate() {
        if !groups[idx] {
            continue;
        }
        let mut parts = vec![*part];
        if *part == "config" {
            parts.extend(["defaultconfigs", "kubejs", "scripts"]);
        }
        for part in parts {
            let path = game.join(part);
            if path.is_dir() {
                use std::os::windows::fs::MetadataExt;
                ensure!(
                    fs::symlink_metadata(&path)?.file_attributes() & 0x400 == 0,
                    "不支持导出目录链接"
                );
                visit(&path, &game, &mut |p, rel| {
                    if rel.ends_with(".disabled") {
                        return Ok(());
                    }
                    zip.start_file(format!("overrides/{rel}"), opts)?;
                    io::copy(&mut fs::File::open(p)?, &mut zip)?;
                    Ok(())
                })?;
            }
        }
    }
    zip.finish()?.flush()?;
    out.as_file().sync_all()?;
    out.persist(dest)?;
    Ok(())
}
pub fn component(
    root: &Path,
    item: &Instance,
    loader: Loader,
    build: &str,
    cancel: &std::sync::atomic::AtomicBool,
    report: &(dyn Fn(String) + Sync),
) -> Result<Instance> {
    ensure!(
        item.external.is_none() && library::safe_id(build),
        "此实例或构建号不支持修改"
    );
    let _lock = library::lock_instance(root, item, "请先退出游戏再修改组件")?;
    let mut candidate = item.clone();
    candidate.loader = loader;
    candidate.loader_version = Some(build.to_owned());
    let pinned =
        crate::game::stage_loader(root, &candidate, cancel, report)?.context("未生成加载器配置")?;
    let _library = crate::game::lock_file(&root.join("library.lock"), "实例库正在使用")?;
    let dir = item.path(root);
    let mut latest: Instance = serde_json::from_slice(&fs::read(dir.join("instance.json"))?)?;
    latest.loader = loader;
    latest.loader_version = candidate.loader_version;
    let pin = dir.join("loader.json");
    let previous = match fs::read(&pin) {
        Ok(v) => Some(v),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    library::atomic_write(&pin, &pinned)?;
    if let Err(e) = library::save(root, &latest) {
        if let Some(bytes) = previous {
            library::atomic_write(&pin, &bytes)?;
        } else {
            fs::remove_file(pin)?;
        }
        return Err(e);
    }
    Ok(latest)
}

pub fn add_files(root: &Path, item: &Instance, kind: &str, paths: &[PathBuf]) -> Result<()> {
    let _guard = library::lock_instance(root, item, "请先退出游戏再添加资源")?;
    let dir = resource_dir(root, item, kind)?;
    let ext = if kind == "mods" { "jar" } else { "zip" };
    let stage = tempfile::tempdir_in(&dir)?;
    let mut names = std::collections::HashSet::new();
    let mut publish = vec![];
    for path in paths {
        ensure!(
            path.extension()
                .is_some_and(|v| v.eq_ignore_ascii_case(ext)),
            "请选择 .{ext} 文件"
        );
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .context("文件名无效")?;
        packs::relative(name)?;
        ensure!(names.insert(name.to_lowercase()), "所选文件重名：{name}");
        let tmp = stage.path().join(name);
        let dest = dir.join(name);
        if dest.try_exists()? {
            ensure!(
                sha1_file(path)? == sha1_file(&dest)?,
                "目录已含不同的同名文件：{name}"
            );
            continue;
        }
        fs::copy(path, &tmp)?;
        zip::ZipArchive::new(fs::File::open(&tmp)?).context("文件不是有效的 ZIP/JAR")?;
        publish.push((tmp, dest));
    }
    if kind == "mods" {
        let editor = crate::mods::history::Editor::open(root, item)?;
        let mut edits = crate::mods::history::Edits::new();
        for (source, dest) in publish {
            ensure!(!dest.try_exists()?, "同名模组已存在");
            edits.insert(
                dest.file_name()
                    .and_then(|n| n.to_str())
                    .context("无效文件名")?
                    .into(),
                Some(source),
            );
        }
        return editor.commit("添加本地模组", edits);
    }
    let mut created = vec![];
    for (tmp, dest) in publish {
        if let Err(e) = fs::hard_link(tmp, &dest) {
            for p in created {
                fs::remove_file(p)?;
            }
            return Err(e.into());
        }
        created.push(dest);
    }
    Ok(())
}

pub fn previous_options(
    root: &Path,
    item: &Instance,
    preview: &packs::Preview,
) -> std::collections::HashSet<usize> {
    let state = PackState::read(&item.path(root)).ok().flatten();
    preview
        .files
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            (f.optional()
                && state
                    .as_ref()
                    .is_some_and(|s| s.files.contains_key(&f.path)))
            .then_some(i)
        })
        .collect()
}
