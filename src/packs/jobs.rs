use super::*;
use crate::{
    content::{Upstream, review::Choices},
    library,
    market::Release,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum Source {
    Local(PathBuf),
    Online(Box<Release>),
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Record {
    pub id: String,
    pub title: String,
    source: Source,
    pub origin: Option<Upstream>,
    pub target: Option<String>,
    pub optional: Option<BTreeSet<String>>,
    pub choices: Choices,
    pub failed: Option<String>,
    #[serde(default)]
    pub confirmed: bool,
    #[serde(default)]
    pub launch: bool,
    #[serde(default)]
    pub prefetch: bool,
    pub committed: bool,
    done: bool,
}
pub(crate) struct Job {
    pub dir: PathBuf,
    data: Mutex<Record>,
    _lease: File,
}
fn directory(root: &Path, id: &str) -> Result<PathBuf> {
    ensure!(
        Uuid::parse_str(id).is_ok() && id.len() == 36 && !id.contains(['/', '\\']),
        "任务编号无效"
    );
    Ok(root.join("pack-tasks").join(id))
}
impl Job {
    pub fn create(
        root: &Path,
        source: Source,
        origin: Option<Upstream>,
        target: Option<String>,
    ) -> Result<Arc<Self>> {
        let id = Uuid::new_v4().to_string();
        let dir = directory(root, &id)?;
        fs::create_dir_all(&dir)?;
        let title = match &source {
            Source::Local(p) => p
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            Source::Online(r) => r.name.clone(),
        };
        let record = Record {
            id,
            title,
            source,
            origin,
            target,
            optional: None,
            choices: Choices::new(),
            failed: None,
            committed: false,
            confirmed: false,
            launch: false,
            prefetch: false,
            done: false,
        };
        let job = Self {
            _lease: crate::game::lock_file(&dir.join("lock"), "任务正在使用")?,
            dir,
            data: Mutex::new(record),
        };
        job.change(|_| {})?;
        Ok(Arc::new(job))
    }
    pub fn open(root: &Path, id: &str) -> Result<Arc<Self>> {
        let dir = directory(root, id)?;
        let lease = crate::game::lock_file(&dir.join("lock"), "这个下载任务已在另一窗口打开")?;
        let record: Record = serde_json::from_reader(File::open(dir.join("task.json"))?)?;
        ensure!(record.id == id && !record.done, "任务已经结束或已移除");
        Ok(Arc::new(Self {
            dir,
            data: Mutex::new(record),
            _lease: lease,
        }))
    }
    pub fn record(&self) -> Record {
        self.data.lock().unwrap().clone()
    }
    pub fn change(&self, edit: impl FnOnce(&mut Record)) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        let mut next = data.clone();
        edit(&mut next);
        library::atomic_write(&self.dir.join("task.json"), &serde_json::to_vec(&next)?)?;
        *data = next;
        Ok(())
    }
    pub fn archive(&self, root: &Path, downloads: &Downloads) -> Result<PathBuf> {
        match self.record().source {
            Source::Online(file) => file.archive(root, downloads),
            Source::Local(source) => {
                let path = self.dir.join("archive.zip");
                if path.is_file() {
                    return Ok(path);
                }
                (downloads.report)("正在保存整合包，以便下次继续".into());
                let mut input = File::open(&source)
                    .with_context(|| format!("请将原整合包放回 {}", source.display()))?;
                let mut temp = tempfile::NamedTempFile::new_in(&self.dir)?;
                let mut buf = [0u8; 64 * 1024];
                loop {
                    downloads.check()?;
                    let n = input.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    std::io::Write::write_all(&mut temp, &buf[..n])?;
                }
                temp.as_file().sync_all()?;
                temp.persist(&path)?;
                Ok(path)
            }
        }
    }
    pub fn instance(&self, root: &Path) -> Result<Option<Instance>> {
        let record = self.record();
        let id = record.target.as_ref().unwrap_or(&record.id);
        ensure!(library::safe_id(id), "目标实例编号无效");
        let dir = root.join("instances").join(id);
        let marker = fs::read_to_string(dir.join("pack-task-id")).ok();
        if !record.committed && marker.as_deref() != Some(&record.id) {
            return Ok(None);
        }
        ensure!(
            marker.as_deref() == Some(&record.id),
            "目标实例已移除或已被其他更新替换"
        );
        Ok(Some(serde_json::from_reader(File::open(
            dir.join("instance.json"),
        )?)?))
    }
    pub fn finish(&self) -> Result<()> {
        self.change(|r| r.done = true)
    }
}
pub fn list(root: &Path) -> Result<Vec<Record>> {
    let dir = root.join("pack-tasks");
    if !dir.try_exists()? {
        return Ok(vec![]);
    }
    let mut out = vec![];
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name();
        let Some(id) = id.to_str() else { continue };
        if let Some(retired) = id.strip_prefix(".done-")
            && directory(root, retired).is_ok()
        {
            let dir = entry.path();
            std::thread::spawn(move || {
                let _ = fs::remove_dir_all(dir);
            });
            continue;
        }
        let Ok(path) = directory(root, id) else {
            continue;
        };
        let Ok(f) = File::open(path.join("task.json")) else {
            continue;
        };
        let record: Record = serde_json::from_reader(f)?;
        if record.id == id && !record.done {
            out.push(record);
        } else if record.id == id
            && let Ok(lease) = crate::game::lock_file(&path.join("lock"), "")
        {
            // Completed jobs can only be collected after their preview and worker release the lease.
            let retired = path.with_file_name(format!(".done-{id}"));
            if fs::rename(&path, &retired).is_ok() {
                drop(lease);
                std::thread::spawn(move || {
                    let _ = fs::remove_dir_all(retired);
                });
            }
        }
    }
    out.sort_by(|a, b| a.title.cmp(&b.title).then_with(|| a.id.cmp(&b.id)));
    Ok(out)
}
