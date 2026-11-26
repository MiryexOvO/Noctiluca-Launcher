use crate::{download::Downloads, library::atomic_write};
use anyhow::Result;
use mc_launcher_core::net::download::{Checksum, DownloadTask};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Mutex,
};

// A hit requires both the manifest checksum and the identity of the local file to be unchanged.
#[derive(Serialize, Deserialize, PartialEq, Eq, Clone)]
struct Stamp {
    size: u64,
    created: u64,
    modified: u64,
}
impl Stamp {
    fn from(meta: fs::Metadata) -> Option<Self> {
        (meta.is_file() && meta.file_attributes() & 0x400 == 0).then(|| Self {
            size: meta.len(),
            created: meta.creation_time(),
            modified: meta.last_write_time(),
        })
    }
    fn read(path: &Path) -> Option<Self> {
        Self::from(fs::symlink_metadata(path).ok()?)
    }
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Verified {
    checksum: String,
    stamp: Stamp,
}
type Files = HashMap<PathBuf, Verified>;
pub(super) struct Index {
    path: PathBuf,
    previous: Files,
    current: Files,
    quick: bool,
}
impl Index {
    pub fn open(dir: &Path, quick: bool) -> Self {
        let path = dir.join("launch-files-v1.json");
        let previous = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            previous,
            current: Files::new(),
            quick,
        }
    }
    pub fn batch(
        &mut self,
        downloads: &Downloads,
        root: &Path,
        label: &str,
        tasks: Vec<DownloadTask>,
        objects: bool,
    ) -> Result<()> {
        downloads.check()?;
        for task in &tasks {
            anyhow::ensure!(
                task.destination.strip_prefix(root).is_ok_and(|rel| rel
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_)))),
                "文件超出安装目录"
            );
        }
        // Windows supplies size and timestamps while enumerating an asset bucket.
        let mut stamps = HashMap::new();
        if self.quick && objects && !self.previous.is_empty() {
            let dirs: HashSet<_> = tasks
                .iter()
                .filter_map(|t| t.destination.parent())
                .collect();
            for dir in dirs {
                downloads.check()?;
                if let Ok(entries) = fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        if let Some(stamp) = entry.metadata().ok().and_then(Stamp::from) {
                            stamps.insert(entry.path(), stamp);
                        }
                    }
                }
            }
        }
        let mut remaining = Vec::new();
        let mut seen = HashSet::new();
        let mut hits = 0;
        for task in tasks {
            if !seen.insert(task.destination.clone()) {
                continue;
            }
            if self.quick
                && let Some(Checksum::Sha1(hash)) = &task.checksum
                && let Some(old) = self.previous.get(&task.destination)
                && hash == &old.checksum
            {
                let stamp = if objects {
                    stamps.remove(&task.destination)
                } else {
                    Stamp::read(&task.destination)
                };
                if let Some(stamp) = stamp
                    && stamp == old.stamp
                {
                    self.current.insert(
                        task.destination,
                        Verified {
                            checksum: hash.clone(),
                            stamp,
                        },
                    );
                    hits += 1;
                    continue;
                }
            }
            remaining.push(task);
        }
        if hits > 0 {
            (downloads.report)(format!("{label} · 已复用 {hits} 个已校验文件"));
        }
        if !remaining.is_empty() {
            let verified = Mutex::new(&mut self.current);
            let initial = Mutex::new(HashMap::new());
            downloads.batch_with(
                root,
                label,
                remaining,
                |task| {
                    initial
                        .lock()
                        .unwrap()
                        .insert(task.destination.clone(), Stamp::read(&task.destination));
                },
                |task| {
                    if let Some(Checksum::Sha1(hash)) = &task.checksum
                        && let Some(stamp) = Stamp::read(&task.destination)
                        && initial
                            .lock()
                            .unwrap()
                            .remove(&task.destination)
                            .flatten()
                            .is_none_or(|before| before == stamp)
                    {
                        verified.lock().unwrap().insert(
                            task.destination.clone(),
                            Verified {
                                checksum: hash.clone(),
                                stamp,
                            },
                        );
                    }
                    Ok(())
                },
            )?;
        }
        Ok(())
    }
    pub fn save(self) -> Result<()> {
        if self.current != self.previous {
            atomic_write(&self.path, &serde_json::to_vec(&self.current)?)?;
        }
        Ok(())
    }
}
