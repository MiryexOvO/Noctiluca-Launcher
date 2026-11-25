use crate::{
    library::lock_instance,
    library::{Instance, safe_id},
};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};
use uuid::Uuid;

pub struct Backup {
    pub path: PathBuf,
    pub created: SystemTime,
}

pub fn list(root: &Path, item: &Instance) -> Result<Vec<Backup>> {
    let dir = root.join("backups").join(&item.id);
    fs::create_dir_all(&dir)?;
    let mut backups = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("instance.json").is_file() {
            backups.push(Backup {
                created: entry.metadata()?.modified()?,
                path: entry.path(),
            });
        }
    }
    backups.sort_by_key(|b| std::cmp::Reverse(b.created));
    Ok(backups)
}

pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(from)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            meta.file_attributes() & 0x400 == 0,
            "无法备份目录链接：{}",
            from.display()
        );
    }
    ensure!(!meta.is_symlink(), "无法备份符号链接：{}", from.display());
    if meta.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        ensure!(meta.is_file(), "无法备份特殊文件：{}", from.display());
        fs::copy(from, to).with_context(|| format!("复制失败：{}", from.display()))?;
    }
    Ok(())
}

// Build outside the library: readers only see a complete folder after the same-volume rename.
fn snapshot(root: &Path, source: &Path, dest: &Path, item: &Instance) -> Result<()> {
    fs::create_dir_all(dest.parent().context("缺少目标目录")?)?;
    let temp = tempfile::tempdir_in(root)?;
    for part in ["game", "loader.json"] {
        let src = source.join(part);
        if src.try_exists()? {
            copy_tree(&src, &temp.path().join(part))?;
        }
    }
    fs::write(
        temp.path().join("instance.json"),
        serde_json::to_vec_pretty(item)?,
    )?;
    fs::rename(temp.path(), dest)?;
    Ok(())
}

pub fn create(root: &Path, item: &Instance) -> Result<()> {
    let source = item.path(root);
    let _lock = lock_instance(root, &source, "请先退出游戏再备份")?;
    let dest = root
        .join("backups")
        .join(&item.id)
        .join(Uuid::new_v4().to_string());
    snapshot(root, &source, &dest, item)
}

pub fn duplicate(root: &Path, item: &Instance, backup: Option<&Path>) -> Result<Instance> {
    let live = item.path(root);
    let _lock = lock_instance(root, &live, "请先退出游戏再复制实例")?;
    let source = backup.unwrap_or(&live);
    let mut new: Instance = serde_json::from_slice(&fs::read(source.join("instance.json"))?)?;
    ensure!(
        new.id == item.id && safe_id(&new.version),
        "备份与实例不匹配"
    );
    new.id = Uuid::new_v4().to_string();
    new.name = format!(
        "{} · {}",
        new.name,
        if backup.is_some() { "恢复" } else { "副本" }
    );
    snapshot(root, source, &new.path(root), &new)?;
    Ok(new)
}
