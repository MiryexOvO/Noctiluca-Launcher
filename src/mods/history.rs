use super::{jar_name, local};
use crate::library::{self, Instance};
use anyhow::{Context, Result, ensure};
use mc_launcher_core::io::hash::sha1_file;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub type Edits = BTreeMap<String, Option<PathBuf>>;
#[derive(Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub name: String,
    before: Option<String>,
    after: Option<String>,
    pub old_label: String,
    pub new_label: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Change {
    pub id: Uuid,
    pub time: i64,
    pub title: String,
    pub files: Vec<FileChange>,
    pub undone: bool,
}
#[derive(Serialize, Deserialize)]
struct Pending {
    owner: String,
    token: Uuid,
}
#[derive(Serialize, Deserialize)]
struct Intent {
    directory: PathBuf,
    change: Change,
}
const PENDING: &str = ".noctiluca-mods-pending.json";
fn storage(root: &Path, id: &str) -> PathBuf {
    root.join("instances").join(id).join("mod-history")
}
fn valid_name(s: &str) -> bool {
    jar_name(s.strip_suffix(".disabled").unwrap_or(s)) && crate::packs::relative(s).is_ok()
}
fn fingerprint(p: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(p) {
        Ok(m) => {
            ensure!(
                m.is_file() && m.file_attributes() & 0x400 == 0,
                "不是普通模组文件：{}",
                p.display()
            );
            Ok(Some(sha1_file(p)?))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn copy_atomic(source: &Path, dest: &Path, hash: &str) -> Result<()> {
    let mut tmp = tempfile::NamedTempFile::new_in(dest.parent().context("缺少上级目录")?)?;
    io::copy(&mut fs::File::open(source)?, tmp.as_file_mut())?;
    ensure!(sha1_file(tmp.path())? == hash, "文件已变化，请刷新后重试");
    tmp.as_file().sync_all()?;
    tmp.persist(dest)?;
    Ok(())
}
fn keep(store: &Path, source: &Path, hash: &str) -> Result<String> {
    let blob = store.join("blobs").join(hash);
    if fingerprint(&blob)?.as_deref() != Some(hash) {
        copy_atomic(source, &blob, hash)?;
    }
    Ok(local::describe(&blob)
        .entries
        .into_iter()
        .map(|e| {
            format!(
                "{} {}",
                if e.name.is_empty() { e.id } else { e.name },
                e.version
            )
            .trim()
            .to_owned()
        })
        .collect::<Vec<_>>()
        .join("、"))
}
impl FileChange {
    pub fn action(&self) -> &'static str {
        match (&self.before, &self.after) {
            (None, Some(_)) => "新增",
            (Some(_), None) => "移除",
            _ => "替换",
        }
    }
}
fn describe_change(
    store: &Path,
    name: String,
    old: &Path,
    new: Option<&Path>,
) -> Result<Option<FileChange>> {
    let before = fingerprint(old)?;
    let after = match new {
        Some(p) => fingerprint(p)?,
        None => None,
    };
    if before == after {
        return Ok(None);
    }
    let old_label = before
        .as_ref()
        .map(|h| keep(store, old, h))
        .transpose()?
        .unwrap_or_default();
    let new_label = new
        .zip(after.as_ref())
        .map(|(p, h)| keep(store, p, h))
        .transpose()?
        .unwrap_or_default();
    Ok(Some(FileChange {
        name,
        before,
        after,
        old_label,
        new_label,
    }))
}
pub fn pack_update(live: &Path, staged: &Path) -> Result<()> {
    let before = live.join("game/mods");
    let after = staged.join("game/mods");
    let store = staged.join("mod-history");
    let names = super::installed(&before)?
        .into_iter()
        .chain(super::installed(&after)?)
        .collect::<std::collections::BTreeSet<_>>();
    if names.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(store.join("blobs"))?;
    let mut files = vec![];
    for name in names {
        ensure!(valid_name(&name), "无效的模组文件名");
        if let Some(f) = describe_change(
            &store,
            name.clone(),
            &before.join(&name),
            Some(&after.join(&name)),
        )? {
            files.push(f);
        }
    }
    if !files.is_empty() {
        let c = Change {
            id: Uuid::new_v4(),
            time: chrono::Utc::now().timestamp_micros(),
            title: "整合包变更 · 模组文件".into(),
            files,
            undone: false,
        };
        library::atomic_write(&entry_path(&store, c.id), &serde_json::to_vec(&c)?)?;
    }
    Ok(())
}
fn validate(change: &Change) -> Result<()> {
    let mut seen = HashSet::new();
    for f in &change.files {
        ensure!(
            valid_name(&f.name) && seen.insert(f.name.to_lowercase()),
            "变更记录包含无效文件名"
        );
        for hash in f.before.iter().chain(&f.after) {
            ensure!(
                hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "变更记录校验值无效"
            );
        }
    }
    Ok(())
}
fn entry_path(store: &Path, id: Uuid) -> PathBuf {
    store.join(format!("{id}.json"))
}
pub fn list(root: &Path, item: &Instance) -> Result<Vec<Change>> {
    let store = storage(root, &item.id);
    if !store.try_exists()? {
        return Ok(vec![]);
    }
    let mut entries = vec![];
    for p in fs::read_dir(store)? {
        let p = p?.path();
        if p.extension().is_some_and(|s| s == "json") {
            let c: Change = serde_json::from_slice(&fs::read(&p)?)?;
            validate(&c)?;
            ensure!(
                p.file_stem().and_then(|n| n.to_str()) == Some(&c.id.to_string()),
                "变更记录编号不一致"
            );
            entries.push(c);
        }
    }
    entries.sort_by_key(|c| std::cmp::Reverse((c.time, c.id)));
    Ok(entries)
}
// Validate the whole operation before touching the first file. Replaying either direction is idempotent.
fn restore(dir: &Path, store: &Path, c: &Change, forward: bool, strict: bool) -> Result<()> {
    validate(c)?;
    let mut writes = vec![];
    for f in &c.files {
        let (from, to) = if forward {
            (&f.before, &f.after)
        } else {
            (&f.after, &f.before)
        };
        let path = dir.join(&f.name);
        let actual = fingerprint(&path)?;
        ensure!(
            &actual == from || (!strict && &actual == to),
            "「{}」已被另外修改，请先检查该文件",
            f.name
        );
        if &actual != to {
            if let Some(hash) = to {
                ensure!(
                    fingerprint(&store.join("blobs").join(hash))?.as_ref() == Some(hash),
                    "缺少完整的恢复文件：{}",
                    f.name
                );
            }
            writes.push((path, from, to));
        }
    }
    for (path, from, to) in writes {
        ensure!(
            &fingerprint(&path)? == from,
            "文件在操作期间已变化：{}",
            path.display()
        );
        if let Some(hash) = to {
            copy_atomic(&store.join("blobs").join(hash), &path, hash)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}
fn recover_locked(root: &Path, dir: &Path) -> Result<()> {
    let marker = dir.join(PENDING);
    if !marker.try_exists()? {
        return Ok(());
    }
    let p: Pending = serde_json::from_slice(&fs::read(&marker)?)?;
    ensure!(library::safe_id(&p.owner), "未完成记录的实例编号无效");
    let store = storage(root, &p.owner);
    let private = store.join(format!("{}.pending", p.token));
    let intent: Intent = serde_json::from_slice(
        &fs::read(&private).context("请在原来的启动器数据目录中恢复未完成的模组操作")?,
    )?;
    ensure!(
        dunce::canonicalize(dir)? == intent.directory,
        "未完成的模组操作属于另一个目录"
    );
    let c = intent.change;
    let entry = entry_path(&store, c.id);
    let committed = if entry.try_exists()? {
        let saved: Change = serde_json::from_slice(&fs::read(entry)?)?;
        saved.undone == c.undone
    } else {
        false
    };
    if !committed {
        restore(dir, &store, &c, c.undone, false)?;
    }
    fs::remove_file(marker)?;
    fs::remove_file(private)?;
    Ok(())
}
pub fn recover(root: &Path, dir: &Path) -> Result<()> {
    if dir.join(PENDING).try_exists()? {
        let _lock = crate::game::lock_file(
            &dir.join(".noctiluca-import.lock"),
            "另一个实例正在更改模组",
        )?;
        recover_locked(root, dir)?;
    }
    Ok(())
}
pub struct Editor<'a> {
    pub dir: PathBuf,
    store: PathBuf,
    owner: &'a str,
    root: &'a Path,
    _lock: fs::File,
}
impl<'a> Editor<'a> {
    // Caller holds the instance lock. This second lock also covers junctions shared by multiple instances.
    pub fn open(root: &'a Path, item: &'a Instance) -> Result<Self> {
        let dir = item.game_dir(root)?.join("mods");
        fs::create_dir_all(&dir)?;
        let lock = crate::game::lock_file(
            &dir.join(".noctiluca-import.lock"),
            "另一个实例正在更改模组",
        )?;
        recover_locked(root, &dir)?;
        let store = storage(root, &item.id);
        fs::create_dir_all(store.join("blobs"))?;
        Ok(Self {
            dir,
            store,
            owner: &item.id,
            root,
            _lock: lock,
        })
    }
    pub fn commit(&self, title: &str, edits: Edits) -> Result<()> {
        let mut files = vec![];
        for (name, source) in edits {
            ensure!(valid_name(&name), "无效的模组文件名");
            let path = self.dir.join(&name);
            if let Some(source) = &source {
                ensure!(source.is_file(), "缺少待安装模组");
            }
            if let Some(f) = describe_change(&self.store, name, &path, source.as_deref())? {
                files.push(f);
            }
        }
        if files.is_empty() {
            return Ok(());
        }
        self.publish(Change {
            id: Uuid::new_v4(),
            time: chrono::Utc::now().timestamp_micros(),
            title: title.into(),
            files,
            undone: false,
        })
    }
    fn publish(&self, c: Change) -> Result<()> {
        let marker = self.dir.join(PENDING);
        let marker_ref = Pending {
            owner: self.owner.into(),
            token: Uuid::new_v4(),
        };
        let private = self.store.join(format!("{}.pending", marker_ref.token));
        let p = Intent {
            directory: dunce::canonicalize(&self.dir)?,
            change: c,
        };
        validate(&p.change)?;
        // A stale undo must be rejected before a recovery marker can be created.
        for f in &p.change.files {
            let expected = if p.change.undone { &f.after } else { &f.before };
            ensure!(
                &fingerprint(&self.dir.join(&f.name))? == expected,
                "「{}」已被另外修改",
                f.name
            );
        }
        library::atomic_write(&private, &serde_json::to_vec(&p)?)?;
        library::atomic_write(&marker, &serde_json::to_vec(&marker_ref)?)?;
        let result =
            restore(&self.dir, &self.store, &p.change, !p.change.undone, true).and_then(|()| {
                library::atomic_write(
                    &entry_path(&self.store, p.change.id),
                    &serde_json::to_vec(&p.change)?,
                )
            });
        if let Err(err) = result {
            recover_locked(self.root, &self.dir)?;
            return Err(err);
        }
        fs::remove_file(marker)?;
        fs::remove_file(private)?;
        Ok(())
    }
}
pub fn undo(root: &Path, item: &Instance, id: Uuid) -> Result<()> {
    let _instance = library::lock_instance(root, item, "请先退出游戏再撤销")?;
    let editor = Editor::open(root, item)?;
    let mut c = list(root, item)?
        .into_iter()
        .find(|c| !c.undone)
        .context("没有可撤销的改动")?;
    ensure!(c.id == id, "最近改动已变化，请刷新后重试");
    c.undone = true;
    editor.publish(c)
}
