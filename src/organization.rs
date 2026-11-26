use crate::library::{self, Instance};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct Entry {
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub played: i64,
    #[serde(default)]
    pub group: String,
}
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Organization(BTreeMap<String, Entry>);
impl Organization {
    pub fn read(root: &Path) -> Result<Self> {
        match fs::read(root.join("instance-library.json")) {
            Ok(v) => Ok(serde_json::from_slice(&v)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn edit(root: &Path, id: &str, edit: impl FnOnce(&mut Entry)) -> Result<Self> {
        ensure!(library::safe_id(id), "无效的实例编号");
        let lock = fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("instance-library.lock"))?;
        lock.lock()?;
        let mut all = Self::read(root)?;
        let e = all.0.entry(id.to_owned()).or_default();
        edit(e);
        ensure!(
            e.group.chars().count() <= 50 && !e.group.chars().any(char::is_control),
            "分组名最多 50 个字符"
        );
        library::atomic_write(
            &root.join("instance-library.json"),
            &serde_json::to_vec(&all)?,
        )?;
        Ok(all)
    }
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.0.get(id)
    }
    pub fn group(&self, id: &str) -> &str {
        self.get(id).map_or("", |e| &e.group)
    }
    pub fn groups(&self, items: &[Instance]) -> BTreeSet<String> {
        items
            .iter()
            .map(|i| self.group(&i.id))
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }
    pub fn order(&self, items: &[Instance]) -> Vec<usize> {
        use std::cmp::Reverse;
        let mut rows = (0..items.len()).collect::<Vec<_>>();
        rows.sort_by_cached_key(|&i| {
            let item = &items[i];
            let e = self.get(&item.id);
            (
                Reverse(e.is_some_and(|v| v.pinned)),
                Reverse(e.map_or(0, |v| v.played)),
                item.name.to_lowercase(),
                item.id.clone(),
            )
        });
        rows
    }
}
