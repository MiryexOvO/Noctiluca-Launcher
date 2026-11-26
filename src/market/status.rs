use super::{Kind, Release, Source};
use crate::library::Instance;
use anyhow::Result;
use chrono::{DateTime, Utc};
use mc_launcher_core::io::hash::sha1_file;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::ErrorKind,
    path::Path,
};

#[derive(Clone, Default)]
pub struct Metadata {
    pub version: String,
    pub games: Vec<String>,
    pub loaders: Vec<String>,
    pub published: Option<DateTime<Utc>>,
}
impl Metadata {
    fn supports(&self, kind: Kind, item: &Instance) -> bool {
        self.games.contains(&item.version)
            && (kind != Kind::Mod
                || self
                    .loaders
                    .iter()
                    .any(|v| v.eq_ignore_ascii_case(item.loader.label())))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalFile {
    pub name: String,
    pub hash: String,
}
#[derive(Default)]
pub struct Installed {
    pub files: Vec<LocalFile>,
    versions: Vec<String>,
    next: Option<String>,
    compatible: bool,
}
impl Installed {
    pub fn label(&self) -> String {
        match self.files.as_slice() {
            [] if self.compatible => "未安装".into(),
            [] => "未安装 · 无兼容版本".into(),
            [file] => {
                let state = if file.name.to_ascii_lowercase().ends_with(".disabled") {
                    "已停用"
                } else {
                    "已安装"
                };
                let mut label = format!("{state} {}", self.versions[0]);
                if let Some(next) = &self.next {
                    label.push_str(&format!(" · 可更新至 {next}"));
                }
                label
            }
            files => format!(
                "发现 {} 个已安装文件 · {}",
                files.len(),
                self.versions.join(" / ")
            ),
        }
    }
}

pub struct Catalog {
    kind: Kind,
    releases: Vec<Release>,
    by_hash: HashMap<String, Vec<usize>>,
    sizes: HashSet<u64>,
    any_size: bool,
}
impl Catalog {
    pub fn fetch(source: Source, kind: Kind, project: &str) -> Result<Self> {
        let releases = match source {
            Source::Modrinth => super::files(source, kind, None, "", project)?,
            Source::CurseForge => crate::curse::releases(kind, None, "", project, true)?,
        };
        Ok(Self::new(kind, releases))
    }
    pub fn new(kind: Kind, releases: Vec<Release>) -> Self {
        let mut by_hash: HashMap<String, Vec<usize>> = HashMap::new();
        let mut sizes = HashSet::new();
        let mut any_size = false;
        for (i, r) in releases.iter().enumerate() {
            by_hash
                .entry(r.sha1.to_ascii_lowercase())
                .or_default()
                .push(i);
            if let Some(n) = r.size {
                sizes.insert(n);
            } else {
                any_size = true;
            }
        }
        Self {
            kind,
            releases,
            by_hash,
            sizes,
            any_size,
        }
    }
    pub fn scan(&self, game: &Path) -> Result<Vec<LocalFile>> {
        let folder = self
            .kind
            .content_folder()
            .ok_or_else(|| anyhow::anyhow!("整合包使用独立实例"))?;
        let ext = if self.kind == Kind::Mod {
            ".jar"
        } else {
            ".zip"
        };
        let mut found = Vec::new();
        let entries = match fs::read_dir(game.join(folder)) {
            Ok(v) => v,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(found),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let lower = name.to_ascii_lowercase();
            if !lower
                .strip_suffix(".disabled")
                .unwrap_or(&lower)
                .ends_with(ext)
            {
                continue;
            }
            let meta = fs::metadata(entry.path())?;
            if !meta.is_file() || (!self.any_size && !self.sizes.contains(&meta.len())) {
                continue;
            }
            let hash = sha1_file(entry.path())?.to_ascii_lowercase();
            if self.by_hash.contains_key(&hash) {
                found.push(LocalFile { name, hash });
            }
        }
        found.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(found)
    }
    pub fn status(&self, item: &Instance, files: &[LocalFile]) -> Installed {
        let compatible = |r: &Release| r.metadata.supports(self.kind, item);
        let latest = self
            .releases
            .iter()
            .filter(|r| compatible(r) && !r.url.is_empty())
            .max_by_key(|r| r.metadata.published);
        let mut state = Installed {
            files: files.to_owned(),
            compatible: latest.is_some(),
            ..Default::default()
        };
        for file in files {
            let installed = self.by_hash[&file.hash]
                .iter()
                .map(|&i| &self.releases[i])
                .max_by_key(|r| (compatible(r), r.metadata.published))
                .expect("scan only returns indexed hashes");
            state.versions.push(installed.metadata.version.clone());
            if let Some(new) = latest
                && new.sha1.to_ascii_lowercase() != file.hash
                && installed.metadata.published.is_some()
                && new.metadata.published > installed.metadata.published
            {
                state.next = Some(new.metadata.version.clone());
            }
        }
        state
    }

    pub fn instances(
        &self,
        root: &Path,
        instances: &[Instance],
    ) -> HashMap<String, Option<Installed>> {
        let mut dirs = HashMap::new();
        instances
            .iter()
            .map(|item| {
                let state = item.game_dir(root).ok().and_then(|game| {
                    dirs.entry(game.clone())
                        .or_insert_with(|| self.scan(&game).ok())
                        .as_deref()
                        .map(|found| self.status(item, found))
                });
                (item.id.clone(), state)
            })
            .collect()
    }
}

// A resource folder can be a junction, or may first be created at install time.
fn directory_identity(path: &Path) -> Option<(same_file::Handle, Vec<std::ffi::OsString>)> {
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match fs::metadata(ancestor) {
            Ok(meta) if meta.is_dir() => {
                return Some((same_file::Handle::from_path(ancestor).ok()?, missing));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                missing.push(ancestor.file_name()?.to_owned());
                ancestor = ancestor.parent()?;
            }
            _ => return None,
        }
    }
}

pub fn sharing_folder(
    root: &Path,
    items: &[Instance],
    target: &Instance,
    kind: Kind,
) -> Vec<String> {
    let Some(folder) = kind.content_folder() else {
        return Vec::new();
    };
    let identity = |item: &Instance| directory_identity(&item.game_dir(root).ok()?.join(folder));
    let Some(chosen) = identity(target) else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|item| item.id != target.id && identity(item).as_ref() == Some(&chosen))
        .map(|item| item.name.clone())
        .collect()
}
