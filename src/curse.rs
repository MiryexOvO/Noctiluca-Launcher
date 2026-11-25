use crate::{
    download,
    library::Loader,
    market::{Hit, Release},
    packs::PackFile,
};
use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, de::DeserializeOwned};
use std::collections::HashMap;
const API: &str = "https://api.curseforge.com/v1";

fn api() -> Result<(Client, reqwest::header::HeaderValue)> {
    let key = crate::setting("CURSEFORGE_API_KEY").and_then(|k|k.into_string().ok())
        .filter(|k| !k.trim().is_empty())
        .context(
            "使用 CurseForge 需要设置 NOCTILUCA_CURSEFORGE_API_KEY，可在 CurseForge for Studios 申请",
        )?;
    let mut key = reqwest::header::HeaderValue::from_str(key.trim())?;
    key.set_sensitive(true);
    Ok((
        download::client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        key,
    ))
}
#[derive(Deserialize)]
struct Response<T> {
    data: T,
}
fn get<T: DeserializeOwned>(request: RequestBuilder) -> Result<T> {
    Ok(request
        .send()?
        .error_for_status()?
        .json::<Response<T>>()?
        .data)
}
fn loader_id(loader: Loader) -> Result<&'static str> {
    match loader {
        Loader::NeoForge => Ok("6"),
        Loader::Forge => Ok("1"),
        Loader::Fabric => Ok("4"),
        _ => bail!("无效的 CurseForge 加载器"),
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: u64,
    name: String,
    summary: String,
    class_id: Option<u32>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    id: u64,
    mod_id: u64,
    display_name: String,
    file_name: String,
    download_url: Option<String>,
    hashes: Vec<Hash>,
}
#[derive(Deserialize)]
struct Hash {
    algo: u32,
    value: String,
}
impl File {
    fn sha1(&self) -> Result<String> {
        self.hashes
            .iter()
            .find(|h| {
                h.algo == 1 && h.value.len() == 40 && h.value.bytes().all(|c| c.is_ascii_hexdigit())
            })
            .map(|h| h.value.clone())
            .context("CurseForge 文件缺少 SHA-1")
    }
    fn url(&self) -> Result<String> {
        self.download_url
            .clone()
            .filter(|s| !s.is_empty())
            .with_context(|| {
                format!(
                    "作者未开放第三方下载：{}（项目 {}，文件 {}）",
                    self.display_name, self.mod_id, self.id
                )
            })
    }
}
pub fn search(loader: Loader, mc: &str, query: &str, offset: usize) -> Result<Vec<Hit>> {
    let (http, key) = api()?;
    let mut request = http
        .get(format!("{API}/mods/search"))
        .header("x-api-key", key)
        .query(&[
            ("gameId", "432"),
            ("classId", "4471"),
            ("modLoaderType", loader_id(loader)?),
            ("searchFilter", query),
            ("pageSize", "10"),
            ("index", &offset.to_string()),
            ("sortField", "6"),
            ("sortOrder", "desc"),
        ]);
    if !mc.is_empty() {
        request = request.query(&[("gameVersion", mc)]);
    }
    let projects: Vec<Project> = get(request)?;
    Ok(projects
        .into_iter()
        .map(|p| Hit {
            id: p.id.to_string(),
            title: p.name,
            description: p.summary,
        })
        .collect())
}
pub fn releases(loader: Loader, mc: &str, id: &str) -> Result<Vec<Release>> {
    let (http, key) = api()?;
    let id = id.parse::<u64>()?;
    let mut releases = vec![];
    let mut index = 0;
    loop {
        let mut request = http
            .get(format!("{API}/mods/{id}/files"))
            .header("x-api-key", &key)
            .query(&[
                ("modLoaderType", loader_id(loader)?),
                ("pageSize", "50"),
                ("index", &index.to_string()),
            ]);
        if !mc.is_empty() {
            request = request.query(&[("gameVersion", mc)]);
        }
        let files: Vec<File> = get(request)?;
        let count = files.len();
        for f in files {
            if f.file_name.to_ascii_lowercase().ends_with(".zip") && f.url().is_ok() {
                releases.push(Release {
                    id: f.id.to_string(),
                    name: f.display_name.clone(),
                    url: f.url()?,
                    sha1: f.sha1()?,
                });
            }
        }
        index += count;
        if count < 50 || index >= 10000 {
            break;
        }
    }
    Ok(releases)
}
#[derive(Deserialize)]
pub(super) struct Requirement {
    #[serde(rename = "projectID")]
    project: u64,
    #[serde(rename = "fileID")]
    file: u64,
    required: bool,
}
pub(super) fn resolve(requirements: Vec<Requirement>) -> Result<Vec<PackFile>> {
    let required: Vec<_> = requirements.into_iter().filter(|r| r.required).collect();
    if required.is_empty() {
        return Ok(vec![]);
    }
    let (http, key) = api()?;
    let mut resolved = vec![];
    for chunk in required.chunks(50) {
        let ids: Vec<_> = chunk.iter().map(|r| r.file).collect();
        let projects: Vec<_> = chunk.iter().map(|r| r.project).collect();
        let files: Vec<File> = get(http
            .post(format!("{API}/mods/files"))
            .header("x-api-key", &key)
            .json(&serde_json::json!({"fileIds":ids})))?;
        let mods: Vec<Project> = get(http
            .post(format!("{API}/mods"))
            .header("x-api-key", &key)
            .json(&serde_json::json!({"modIds":projects})))?;
        for r in chunk {
            let file = files
                .iter()
                .find(|f| f.id == r.file && f.mod_id == r.project)
                .with_context(|| format!("CurseForge 缺少文件 {} / {}", r.project, r.file))?;
            let project = mods
                .iter()
                .find(|p| p.id == r.project)
                .context("CurseForge 项目信息缺失")?;
            let folder = match project.class_id {
                Some(6) => "mods",
                Some(12) => "resourcepacks",
                Some(6552) => "shaderpacks",
                other => bail!(
                    "整合包包含暂不支持的内容类型 {:?}（{}）",
                    other,
                    project.name
                ),
            };
            ensure!(
                !file.file_name.contains(['/', char::from(92)]),
                "无效文件名"
            );
            resolved.push(PackFile {
                path: format!("{folder}/{}", file.file_name),
                downloads: vec![file.url()?],
                hashes: HashMap::from([("sha1".into(), file.sha1()?)]),
                env: HashMap::new(),
            });
        }
    }
    Ok(resolved)
}
