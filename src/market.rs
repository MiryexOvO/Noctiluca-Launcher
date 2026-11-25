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
#[derive(Clone)]
pub struct Hit {
    pub id: String,
    pub title: String,
    pub description: String,
}
#[derive(Clone)]
pub struct Release {
    pub source: Source,
    pub project: String,
    pub name: String,
    pub id: String,
    pub url: String,
    pub sha1: String,
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
}
#[derive(Deserialize)]
struct Version {
    id: String,
    name: String,
    version_number: String,
    files: Vec<PackAsset>,
}
#[derive(Deserialize)]
struct PackAsset {
    filename: String,
    url: String,
    hashes: Hashes,
    primary: bool,
}
#[derive(Deserialize)]
struct Hashes {
    sha1: String,
}

pub fn search(
    source: Source,
    loader: Loader,
    mc: &str,
    query: &str,
    offset: usize,
) -> Result<Vec<Hit>> {
    if source == Source::CurseForge {
        return crate::curse::search(loader, mc, query, offset);
    }
    let mut facets = vec![
        vec!["project_type:modpack".to_owned()],
        vec![format!("categories:{}", slug(loader)?)],
    ];
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
        })
        .collect())
}
pub fn releases(source: Source, loader: Loader, mc: &str, project: &str) -> Result<Vec<Release>> {
    if source == Source::CurseForge {
        return crate::curse::releases(loader, mc, project);
    }
    ensure!(safe_id(project), "项目编号无效");
    let mut request = download::client()?
        .get(format!(
            "https://api.modrinth.com/v2/project/{project}/version"
        ))
        .query(&[("loaders", serde_json::to_string(&[slug(loader)?])?)]);
    if !mc.is_empty() {
        request = request.query(&[("game_versions", serde_json::to_string(&[mc])?)]);
    }
    let versions: Vec<Version> = request.send()?.error_for_status()?.json()?;
    Ok(versions
        .into_iter()
        .filter_map(|v| {
            let file = v
                .files
                .into_iter()
                .filter(|f| f.filename.ends_with(".mrpack"))
                .max_by_key(|f| f.primary)?;
            Some(Release {
                source,
                project: project.into(),
                id: v.id,
                name: format!("{} · {}", v.name, v.version_number),
                url: file.url,
                sha1: file.hashes.sha1,
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
        Loader::Fabric => {
            #[derive(Deserialize)]
            struct Entry {
                loader: Build,
            }
            #[derive(Deserialize)]
            struct Build {
                version: String,
            }
            let list: Vec<Entry> = download::client()?
                .get(format!("https://meta.fabricmc.net/v2/versions/loader/{mc}"))
                .send()?
                .error_for_status()?
                .json()?;
            list.into_iter().map(|v| v.loader.version).collect()
        }
        Loader::Quilt => mc_launcher_core::loader::quilt::list_loader_versions()?
            .into_iter()
            .map(|v| v.version)
            .collect(),
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
fn resource_type(kind: &str) -> Result<&str> {
    match kind {
        "resourcepacks" => Ok("resourcepack"),
        "shaderpacks" => Ok("shader"),
        _ => bail!("不支持此资源类型"),
    }
}
pub fn resources(kind: &str, mc: &str, q: &str) -> Result<Vec<Hit>> {
    let filters = serde_json::json!([
        [format!("project_type:{}", resource_type(kind)?)],
        [format!("versions:{mc}")]
    ])
    .to_string();
    let v: Search = download::client()?
        .get("https://api.modrinth.com/v2/search")
        .query(&[("query", q), ("facets", &filters), ("limit", "10")])
        .send()?
        .error_for_status()?
        .json()?;
    Ok(v.hits
        .into_iter()
        .map(|p| Hit {
            id: p.project_id,
            title: p.title,
            description: p.description,
        })
        .collect())
}
pub fn install_resource(
    root: &Path,
    item: &crate::library::Instance,
    kind: &str,
    id: &str,
    cancel: &std::sync::atomic::AtomicBool,
    report: impl Fn(String) + Sync,
) -> Result<()> {
    resource_type(kind)?;
    ensure!(safe_id(id), "项目 ID 无效");
    let _lock = crate::library::lock_instance(root, item, "请退出游戏后安装资源")?;
    let dir = crate::content::resource_dir(root, item, kind)?;
    let d = Downloads::new(cancel, &report)?;
    let versions: Vec<Version> = d
        .http
        .get(format!("https://api.modrinth.com/v2/project/{id}/version"))
        .query(&[(
            "game_versions",
            serde_json::json!([item.version]).to_string(),
        )])
        .send()?
        .error_for_status()?
        .json()?;
    let file = versions
        .into_iter()
        .find_map(|v| {
            v.files
                .into_iter()
                .filter(|f| f.filename.to_ascii_lowercase().ends_with(".zip"))
                .max_by_key(|f| f.primary)
        })
        .ok_or_else(|| anyhow::anyhow!("没有兼容的 ZIP 资源"))?;
    let name = crate::packs::relative(&file.filename)?;
    ensure!(name.components().count() == 1, "文件名无效");
    let url = url::Url::parse(&file.url)?;
    ensure!(
        url.scheme() == "https" && url.host_str() == Some("cdn.modrinth.com"),
        "资源下载地址无效"
    );
    let dest = dir.join(name);
    if dest.exists() {
        ensure!(
            mc_launcher_core::io::hash::sha1_file(&dest)? == file.hashes.sha1,
            "同名文件已经存在"
        );
        return Ok(());
    }
    d.batch(
        &dir,
        "安装资源",
        vec![DownloadTask {
            url: file.url,
            label: file.filename,
            destination: dest,
            checksum: Some(Checksum::Sha1(file.hashes.sha1)),
        }],
    )
}
