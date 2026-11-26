use crate::{
    library::{self, Instance},
    packs,
};
use anyhow::{Context, Result, bail, ensure};
use mc_launcher_core::io::hash::sha1_file;
use std::{
    collections::HashSet,
    fs::{self, File},
    io,
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
};
use zip::ZipArchive;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Mod,
    Resource,
    Shader,
}
impl Kind {
    pub fn dir(self) -> &'static str {
        match self {
            Self::Mod => "mods",
            Self::Resource => "resourcepacks",
            Self::Shader => "shaderpacks",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Mod => "模组",
            Self::Resource => "资源包",
            Self::Shader => "光影",
        }
    }
}

#[derive(Clone)]
pub(crate) struct Row {
    pub path: PathBuf,
    pub name: String,
    pub kind: Option<Kind>,
    pub ambiguous: bool,
    pub skip: bool,
    pub replace: bool,
    source_hash: String,
    destinations: Vec<(Kind, Option<String>)>,
}
impl Row {
    fn existing(&self) -> Option<&str> {
        self.destinations
            .iter()
            .find(|(k, _)| Some(*k) == self.kind)
            .and_then(|(_, v)| v.as_deref())
    }
    pub fn conflict(&self) -> bool {
        self.existing().is_some()
    }
    pub fn select(&mut self, kind: Kind) {
        self.kind = Some(kind);
        self.replace = false;
        self.skip = self.conflict();
    }
}

#[derive(Clone)]
pub(crate) struct Plan {
    pub item: Instance,
    pub rows: Vec<Row>,
    game: PathBuf,
    dirs: Vec<(Kind, PathBuf)>,
    pub sharing: Vec<(Kind, Vec<String>)>,
}
pub(crate) enum Drop {
    Pack(PathBuf),
    Files(Box<Plan>),
}

enum ArchiveKind {
    Modpack,
    Resource(Option<Kind>),
}

fn classify(path: &Path) -> Result<ArchiveKind> {
    if path
        .extension()
        .is_some_and(|v| v.eq_ignore_ascii_case("mrpack"))
    {
        return Ok(ArchiveKind::Modpack);
    }
    if path
        .extension()
        .is_some_and(|v| v.eq_ignore_ascii_case("jar"))
    {
        return Ok(ArchiveKind::Resource(Some(Kind::Mod)));
    }
    ensure!(
        path.extension()
            .is_some_and(|v| v.eq_ignore_ascii_case("zip")),
        "请选择 JAR、ZIP 或 MRPACK 文件"
    );
    let zip = ZipArchive::new(File::open(path)?).context("读取 ZIP 失败")?;
    if zip.index_for_name("modrinth.index.json").is_some()
        || zip
            .file_names()
            .any(|n| n == "mmc-pack.json" || n.ends_with("/mmc-pack.json"))
        || (zip.index_for_name("manifest.json").is_some() && packs::is_pack_archive(path))
    {
        return Ok(ArchiveKind::Modpack);
    }
    let resource = zip.index_for_name("pack.mcmeta").is_some()
        && zip.file_names().any(|n| n.starts_with("assets/"));
    let shader = zip.file_names().any(|n| {
        let mut parts = n.split('/');
        parts.next() == Some("shaders") || parts.next() == Some("shaders")
    });
    Ok(ArchiveKind::Resource(match (resource, shader) {
        (true, false) => Some(Kind::Resource),
        (false, true) => Some(Kind::Shader),
        _ => None,
    }))
}

fn resolved_dir(path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            let resolved = dunce::canonicalize(path)?;
            ensure!(resolved.is_dir(), "资源目录不可用：{}", path.display());
            Ok(resolved)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().context("找不到实例目录")?;
            Ok(resolved_dir(parent)?.join(path.file_name().context("无效目录")?))
        }
        Err(e) => Err(e.into()),
    }
}

fn fingerprint(dir: &Path, name: &str) -> Result<Option<String>> {
    let file = dir.join(name);
    match fs::symlink_metadata(&file) {
        Ok(meta) => {
            ensure!(
                meta.is_file() && meta.file_attributes() & 0x400 == 0,
                "目标不是普通文件：{name}"
            );
            Ok(Some(sha1_file(&file)?))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn preview(
    root: &Path,
    item: &Instance,
    paths: &[PathBuf],
    instances: &[Instance],
) -> Result<Drop> {
    let game = item.game_dir(root)?;
    let mut rows = Vec::new();
    let mut dirs = Vec::<(Kind, PathBuf)>::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .context("无效的文件名")?
            .to_owned();
        packs::relative(&name)?;
        let ArchiveKind::Resource(kind) =
            classify(path).with_context(|| format!("无法识别 {name}"))?
        else {
            ensure!(paths.len() == 1, "整合包请单独拖入；这批文件尚未安装");
            return Ok(Drop::Pack(path.clone()));
        };
        let mut destinations = vec![];
        for &candidate in kind
            .as_ref()
            .map_or(&[Kind::Resource, Kind::Shader][..], std::slice::from_ref)
        {
            let dir = if let Some((_, path)) = dirs.iter().find(|(k, _)| *k == candidate) {
                path
            } else {
                dirs.push((candidate, resolved_dir(&game.join(candidate.dir()))?));
                &dirs.last().unwrap().1
            };
            destinations.push((candidate, fingerprint(dir, &name)?));
        }
        let mut row = Row {
            path: path.clone(),
            name,
            kind,
            ambiguous: kind.is_none(),
            skip: false,
            replace: false,
            source_hash: sha1_file(path)?,
            destinations,
        };
        row.skip = row.conflict();
        rows.push(row);
    }
    use crate::market;
    let sharing = dirs
        .iter()
        .filter_map(|(k, _)| {
            let kind = match k {
                Kind::Mod => market::Kind::Mod,
                Kind::Resource => market::Kind::Resource,
                Kind::Shader => market::Kind::Shader,
            };
            let names = market::status::sharing_folder(root, instances, item, kind);
            (!names.is_empty()).then_some((*k, names))
        })
        .collect();
    Ok(Drop::Files(Box::new(Plan {
        dirs,
        sharing,
        item: item.clone(),
        rows,
        game,
    })))
}

impl Plan {
    fn target(&self, kind: Kind) -> Result<&Path> {
        let (_, pinned) = self
            .dirs
            .iter()
            .find(|(k, _)| *k == kind)
            .context("目标类型已变更")?;
        ensure!(
            resolved_dir(&self.game.join(kind.dir()))? == *pinned,
            "资源目录已变更，请重新拖入文件"
        );
        Ok(pinned)
    }

    pub fn selection(&self) -> Result<usize> {
        let mut seen = HashSet::new();
        for row in self.rows.iter().filter(|r| !r.skip) {
            let kind = row.kind.context("请为未识别的 ZIP 选择类型，或跳过它")?;
            ensure!(
                seen.insert((kind.dir(), row.name.to_lowercase())),
                "有多个文件要写入同一个位置，请只保留一个：{}",
                row.name
            );
            ensure!(
                !row.conflict() || row.replace,
                "请选择替换或跳过：{}",
                row.name
            );
        }
        ensure!(!seen.is_empty(), "没有要安装的文件");
        Ok(seen.len())
    }

    pub fn apply(&self, root: &Path) -> Result<usize> {
        let count = self.selection()?;
        let _lock = library::lock_instance(root, &self.item, "请退出游戏再导入资源")?;
        let current: Instance =
            serde_json::from_reader(File::open(self.item.path(root).join("instance.json"))?)?;
        ensure!(
            current.id == self.item.id && current.game_dir(root)? == self.game,
            "实例位置已变更，请重新拖入文件"
        );
        let mut staged = Vec::new();
        for row in self.rows.iter().filter(|r| !r.skip) {
            let kind = row.kind.context("请选择资源类型")?;
            let dir = self.target(kind)?;
            let current = fingerprint(dir, &row.name)?;
            if current.as_ref() == Some(&row.source_hash) {
                continue;
            }
            ensure!(
                current.as_deref() == row.existing(),
                "目标已变更，请重新拖入：{}",
                row.name
            );
            fs::create_dir_all(dir)?;
            let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
            io::copy(&mut File::open(&row.path)?, tmp.as_file_mut())?;
            ensure!(
                sha1_file(tmp.path())? == row.source_hash,
                "源文件已变更，请重新拖入：{}",
                row.name
            );
            staged.push((tmp, kind, row));
        }
        let ready = count - staged.len();
        for (index, (tmp, kind, row)) in staged.into_iter().enumerate() {
            self.publish(tmp, kind, row).with_context(|| {
                format!(
                    "已有 {} / {count} 个文件就绪，写入「{}」失败。处理后可重新安装。",
                    ready + index,
                    row.name
                )
            })?;
        }
        Ok(count)
    }

    fn publish(&self, tmp: tempfile::NamedTempFile, kind: Kind, row: &Row) -> Result<()> {
        let dir = self.target(kind)?;
        let dest = dir.join(&row.name);
        if fingerprint(dir, &row.name)?.as_deref() != row.existing() {
            bail!("目标已变更，请重新检查：{}", row.name)
        }
        if row.replace && row.conflict() {
            tmp.persist(&dest)?;
        } else {
            tmp.persist_noclobber(&dest)?;
        }
        Ok(())
    }
}
