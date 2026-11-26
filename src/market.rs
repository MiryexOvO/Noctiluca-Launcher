pub mod icon;
pub mod status;
use crate::{
    download::{self, Downloads},
    library::{Loader, safe_id},
};
use anyhow::{Result, bail, ensure};
use mc_launcher_core::{
    loader::{forge, neoforge},
    net::download::{Checksum, DownloadTask},
};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, Deserialize)]
pub enum Source {
    Modrinth,
    CurseForge,
}
impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Modrinth => "Modrinth",
            Self::CurseForge => "CurseForge",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mod,
    Pack,
    Resource,
    Shader,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mod => "模组",
            Self::Pack => "整合包",
            Self::Resource => "资源包",
            Self::Shader => "光影",
        }
    }
    fn slug(self) -> &'static str {
        match self {
            Self::Mod => "mod",
            Self::Pack => "modpack",
            Self::Resource => "resourcepack",
            Self::Shader => "shader",
        }
    }
    pub fn class(self) -> &'static str {
        match self {
            Self::Mod => "6",
            Self::Pack => "4471",
            Self::Resource => "12",
            Self::Shader => "6552",
        }
    }
    pub fn content_folder(self) -> Option<&'static str> {
        match self {
            Self::Mod => Some("mods"),
            Self::Resource => Some("resourcepacks"),
            Self::Shader => Some("shaderpacks"),
            Self::Pack => None,
        }
    }
    pub fn uses_loader(self) -> bool {
        matches!(self, Self::Mod | Self::Pack)
    }
    pub fn extension(self, source: Source) -> &'static str {
        match (self, source) {
            (Self::Mod, _) => ".jar",
            (Self::Pack, Source::Modrinth) => ".mrpack",
            _ => ".zip",
        }
    }
}
#[derive(Clone)]
pub struct Hit {
    pub id: String,
    pub title: String,
    pub description: String,
    pub icon_url: Option<String>,
    pub reference_version: Option<String>,
    pub reference_size: Option<u64>,
}
#[derive(Clone)]
pub struct Release {
    pub source: Source,
    pub project: String,
    pub name: String,
    pub id: String,
    pub url: String,
    pub sha1: String,
    pub filename: String,
    pub size: Option<u64>,
    pub metadata: status::Metadata,
}
impl Release {
    pub fn archive(&self, root: &Path, downloads: &Downloads) -> Result<PathBuf> {
        ensure!(
            safe_id(&self.id)
                && self.sha1.len() == 40
                && self.sha1.bytes().all(|c| c.is_ascii_hexdigit()),
            "整合包索引无效"
        );
        let destination = root
            .join("packs")
            .join(format!("{}.zip", self.sha1.to_ascii_lowercase()));
        downloads.batch(
            root,
            "下载整合包",
            vec![DownloadTask {
                label: self.name.clone(),
                url: self.url.clone(),
                destination: destination.clone(),
                checksum: Some(Checksum::Sha1(self.sha1.clone())),
            }],
        )?;
        Ok(destination)
    }
}
fn slug(loader: Loader) -> Result<&'static str> {
    match loader {
        Loader::NeoForge => Ok("neoforge"),
        Loader::Forge => Ok("forge"),
        Loader::Fabric => Ok("fabric"),
        Loader::Quilt => Ok("quilt"),
        Loader::Vanilla => Ok("minecraft"),
    }
}
#[derive(Deserialize)]
struct Search {
    hits: Vec<Project>,
}
#[derive(Deserialize)]
struct Project {
    project_id: String,
    title: String,
    description: String,
    icon_url: Option<String>,
    latest_version: Option<String>,
}
#[derive(Deserialize)]
struct Version {
    id: String,
    name: String,
    version_number: String,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    loaders: Vec<String>,
    date_published: Option<String>,
    files: Vec<PackAsset>,
}
#[derive(Deserialize)]
struct PackAsset {
    filename: String,
    url: String,
    hashes: Hashes,
    primary: bool,
    size: Option<u64>,
}
#[derive(Deserialize)]
struct Hashes {
    sha1: String,
}

pub fn search(
    source: Source,
    kind: Kind,
    loader: Option<Loader>,
    mc: &str,
    query: &str,
    offset: usize,
) -> Result<Vec<Hit>> {
    let loader = loader.filter(|_| kind.uses_loader());
    if source == Source::CurseForge {
        return crate::curse::search_class(loader, mc, query, offset, kind.class());
    }
    let mut facets = vec![vec![format!("project_type:{}", kind.slug())]];
    if let Some(loader) = loader {
        facets.push(vec![format!("categories:{}", slug(loader)?)]);
    }
    if !mc.is_empty() {
        facets.push(vec![format!("versions:{mc}")]);
    }
    let page: Search = download::client()?
        .get("https://api.modrinth.com/v2/search")
        .query(&[
            ("query", query),
            ("facets", &serde_json::to_string(&facets)?),
            ("limit", "10"),
            ("offset", &offset.to_string()),
        ])
        .send()?
        .error_for_status()?
        .json()?;
    Ok(page
        .hits
        .into_iter()
        .map(|p| Hit {
            id: p.project_id,
            title: p.title,
            description: p.description,
            icon_url: p.icon_url,
            reference_version: p.latest_version,
            reference_size: None,
        })
        .collect())
}
fn primary_file(files: Vec<PackAsset>, kind: Kind) -> Option<PackAsset> {
    files
        .into_iter()
        .filter(|f| {
            f.filename
                .to_ascii_lowercase()
                .ends_with(kind.extension(Source::Modrinth))
        })
        .max_by_key(|f| f.primary)
}

pub fn reference_sizes(
    kind: Kind,
    ids: &[String],
) -> Result<std::collections::HashMap<String, u64>> {
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let versions: Vec<Version> = download::client()?
        .get("https://api.modrinth.com/v2/versions")
        .timeout(std::time::Duration::from_secs(10))
        .query(&[("ids", serde_json::to_string(ids)?)])
        .send()?
        .error_for_status()?
        .json()?;
    Ok(versions
        .into_iter()
        .filter_map(|v| Some((v.id, primary_file(v.files, kind)?.size?)))
        .collect())
}

pub fn releases(source: Source, loader: Loader, mc: &str, project: &str) -> Result<Vec<Release>> {
    files(source, Kind::Pack, Some(loader), mc, project)
}

pub fn files(
    source: Source,
    kind: Kind,
    loader: Option<Loader>,
    mc: &str,
    project: &str,
) -> Result<Vec<Release>> {
    let loader = loader.filter(|_| kind.uses_loader());
    if source == Source::CurseForge {
        return crate::curse::releases(kind, loader, mc, project, false);
    }
    ensure!(safe_id(project), "项目编号无效");
    let mut request = download::client()?.get(format!(
        "https://api.modrinth.com/v2/project/{project}/version"
    ));
    if let Some(loader) = loader {
        request = request.query(&[("loaders", serde_json::json!([slug(loader)?]).to_string())]);
    }
    if !mc.is_empty() {
        request = request.query(&[("game_versions", serde_json::to_string(&[mc])?)]);
    }
    let versions: Vec<Version> = request.send()?.error_for_status()?.json()?;
    Ok(versions
        .into_iter()
        .filter_map(|v| {
            let file = primary_file(v.files, kind)?;
            Some(Release {
                source,
                project: project.into(),
                id: v.id,
                name: format!("{} · {}", v.name, v.version_number),
                url: file.url,
                sha1: file.hashes.sha1,
                filename: file.filename,
                size: file.size,
                metadata: status::Metadata {
                    version: v.version_number,
                    games: v.game_versions,
                    loaders: v.loaders,
                    published: v.date_published.and_then(|s| s.parse().ok()),
                },
            })
        })
        .collect())
}
pub fn builds(loader: Loader, mc: &str) -> Result<Vec<String>> {
    ensure!(safe_id(mc), "请填写 Minecraft 版本");
    let mut ids: Vec<String> = match loader {
        Loader::Forge => {
            let prefix = format!("{mc}-");
            forge::list_forge_versions()?
                .iter()
                .filter_map(|s| s.strip_prefix(&prefix).map(str::to_owned))
                .collect()
        }
        Loader::NeoForge => neoforge::list_neoforge_versions()?
            .into_iter()
            .filter(|v| neoforge::latest_for_minecraft(std::slice::from_ref(v), mc).is_ok())
            .collect(),
        Loader::Fabric | Loader::Quilt => {
            #[derive(Deserialize)]
            struct Entry {
                loader: Build,
            }
            #[derive(Deserialize)]
            struct Build {
                version: String,
            }
            let base = if loader == Loader::Fabric {
                "https://meta.fabricmc.net/v2"
            } else {
                "https://meta.quiltmc.org/v3"
            };
            let list: Vec<Entry> = download::client()?
                .get(format!("{base}/versions/loader/{mc}"))
                .send()?
                .error_for_status()?
                .json()?;
            list.into_iter().map(|v| v.loader.version).collect()
        }
        _ => bail!("请选择支持的加载器"),
    };
    ids.retain(|v| safe_id(v));
    // Maven metadata is not guaranteed to be ordered. Compare numeric segments as numbers.
    ids.sort_by_cached_key(|v| {
        let (number, tag) = v.split_once('-').unwrap_or((v, ""));
        (
            number
                .split('.')
                .map(|n| n.parse::<u32>().unwrap_or(0))
                .collect::<Vec<_>>(),
            tag.is_empty(),
            tag.to_owned(),
        )
    });
    ids.reverse();
    ids.dedup();
    Ok(ids)
}

pub fn lookup_pack_hash(hash: &str) -> Result<crate::content::Upstream> {
    ensure!(
        hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "归档校验值无效"
    );
    #[derive(Deserialize)]
    struct Match {
        project_id: String,
        id: String,
    }
    let resp = download::client()?
        .get(format!("https://api.modrinth.com/v2/version_file/{hash}"))
        .query(&[("algorithm", "sha1")])
        .send()?;
    ensure!(
        resp.status() != reqwest::StatusCode::NOT_FOUND,
        "这个本地包没有对应的 Modrinth 发布版本，请选择本地新版包"
    );
    let v: Match = resp.error_for_status()?.json()?;
    Ok(crate::content::Upstream {
        source: Source::Modrinth,
        project: v.project_id,
        release: v.id,
    })
}
fn resource_kind(folder: &str) -> Result<Kind> {
    match folder {
        "resourcepacks" => Ok(Kind::Resource),
        "shaderpacks" => Ok(Kind::Shader),
        _ => bail!("未知资源类型"),
    }
}
pub fn resources(folder: &str, mc: &str, query: &str) -> Result<Vec<Hit>> {
    search(Source::Modrinth, resource_kind(folder)?, None, mc, query, 0)
}

pub fn install_resource(
    root: &Path,
    item: &crate::library::Instance,
    folder: &str,
    id: &str,
    cancel: &std::sync::atomic::AtomicBool,
    report: impl Fn(String) + Sync,
) -> Result<()> {
    let kind = resource_kind(folder)?;
    let release = files(Source::Modrinth, kind, None, &item.version, id)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("没有兼容的 ZIP 资源"))?;
    install_file(root, item, kind, &release, cancel, report)
}

pub fn install_file(
    root: &Path,
    item: &crate::library::Instance,
    kind: Kind,
    file: &Release,
    cancel: &std::sync::atomic::AtomicBool,
    report: impl Fn(String) + Sync,
) -> Result<()> {
    install_choice(root, item, kind, file, None, cancel, report)
}

pub struct Selection {
    pub catalog: std::sync::Arc<status::Catalog>,
    pub seen: Vec<status::LocalFile>,
}

pub fn install_choice(
    root: &Path,
    item: &crate::library::Instance,
    kind: Kind,
    file: &Release,
    selection: Option<&Selection>,
    cancel: &std::sync::atomic::AtomicBool,
    report: impl Fn(String) + Sync,
) -> Result<()> {
    let _lock = crate::library::lock_instance(root, item, "实例正在使用，请稍后重试")?;
    let current: crate::library::Instance =
        serde_json::from_slice(&std::fs::read(item.path(root).join("instance.json"))?)?;
    ensure!(
        current.id == item.id
            && current.version == item.version
            && current.loader == item.loader
            && current.external == item.external,
        "实例已变更，请重新选择目标实例"
    );
    let d = Downloads::new(cancel, &report)?;
    d.check()?;
    let folder = kind
        .content_folder()
        .ok_or_else(|| anyhow::anyhow!("请在整合包预览中安装"))?;
    let game = current.game_dir(root)?;
    if let Some(choice) = selection {
        let found = choice.catalog.scan(&game)?;
        ensure!(found == choice.seen, "目标实例的文件已变更，请重新选择版本");
        if found
            .iter()
            .any(|f| f.hash.eq_ignore_ascii_case(&file.sha1))
        {
            return Ok(());
        }
        if kind == Kind::Mod && !found.is_empty() {
            ensure!(found.len() == 1, "存在多个同项目模组，请先在实例中整理");
            return crate::mods::replace_pinned(&game.join(folder), &current, &found[0], file, &d);
        }
    }
    if kind == Kind::Mod {
        return crate::mods::install_pinned(&game.join(folder), &current, file, &d);
    }
    let dir = crate::content::resource_dir(root, &current, folder)?;
    let name = crate::packs::relative(&file.filename)?;
    ensure!(
        name.components().count() == 1 && file.filename.to_ascii_lowercase().ends_with(".zip"),
        "资源文件名无效"
    );
    let url = url::Url::parse(&file.url)?;
    let trusted = match file.source {
        Source::Modrinth => url.host_str() == Some("cdn.modrinth.com"),
        Source::CurseForge => url.host_str().is_some_and(|h| h.ends_with(".forgecdn.net")),
    };
    ensure!(url.scheme() == "https" && trusted, "资源下载地址无效");
    ensure!(
        file.sha1.len() == 40 && file.sha1.bytes().all(|c| c.is_ascii_hexdigit()),
        "资源缺少有效的 SHA-1"
    );
    let dest = dir.join(name);
    if dest.exists() {
        ensure!(
            mc_launcher_core::io::hash::sha1_file(&dest)?.eq_ignore_ascii_case(&file.sha1),
            "同名文件已经存在"
        );
        return Ok(());
    }
    d.batch(
        &dir,
        "安装资源",
        vec![DownloadTask {
            url: file.url.clone(),
            label: file.filename.clone(),
            destination: dest,
            checksum: Some(Checksum::Sha1(file.sha1.clone())),
        }],
    )
}
