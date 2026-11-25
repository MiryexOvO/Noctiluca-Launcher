use crate::{
    download,
    library::Loader,
    market::{Hit, Kind, Release, Source},
    packs::PackFile,
};
use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, de::DeserializeOwned};
use std::collections::HashMap;
const OFFICIAL: &str = "https://api.curseforge.com/v1";
fn endpoint() -> Result<String> {
    let value = crate::setting("CURSEFORGE_SERVICE")
        .and_then(|v| v.into_string().ok())
        .or_else(|| option_env!("NOCTILUCA_CURSEFORGE_SERVICE").map(str::to_owned))
        .unwrap_or_else(|| OFFICIAL.into());
    let url = url::Url::parse(&value)?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "CurseForge 服务地址无效"
    );
    Ok(value.trim_end_matches('/').to_owned())
}

fn api() -> Result<(Client, String)> {
    let url = endpoint()?;
    let mut headers = reqwest::header::HeaderMap::new();
    if url == OFFICIAL {
        let key=crate::setting("CURSEFORGE_API_KEY").and_then(|v|v.into_string().ok()).filter(|v|!v.trim().is_empty()).context("此发行版尚未连接 Noctiluca 的 CurseForge 服务。可使用 Modrinth 或导入已安装的游戏目录。")?;
        let mut value = reqwest::header::HeaderValue::from_str(key.trim())?;
        value.set_sensitive(true);
        headers.insert("x-api-key", value);
    }
    Ok((
        download::client_builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        url,
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
        Loader::Quilt => Ok("5"),
        _ => bail!("无效的 CurseForge 加载器"),
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: u64,
    slug: String,
    name: String,
    summary: String,
    class_id: Option<u32>,
    logo: Option<Logo>,
    #[serde(default)]
    latest_files: Vec<PreviewFile>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Logo {
    thumbnail_url: Option<String>,
    url: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewFile {
    file_name: String,
    file_date: Option<String>,
    file_length: Option<u64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    id: u64,
    mod_id: u64,
    display_name: String,
    file_date: Option<String>,
    file_name: String,
    file_length: Option<u64>,
    download_url: Option<String>,
    hashes: Vec<Hash>,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    dependencies: Vec<Dep>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Dep {
    mod_id: u64,
    relation_type: u32,
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
pub fn search_class(
    loader: Option<Loader>,
    mc: &str,
    query: &str,
    offset: usize,
    class: &str,
) -> Result<Vec<Hit>> {
    let (http, api) = api()?;
    let mut request = http.get(format!("{api}/mods/search")).query(&[
        ("gameId", "432"),
        ("classId", class),
        ("searchFilter", query),
        ("pageSize", "10"),
        ("index", &offset.to_string()),
        ("sortField", "6"),
        ("sortOrder", "desc"),
    ]);
    if let Some(loader) = loader {
        request = request.query(&[("modLoaderType", loader_id(loader)?)]);
    }
    if !mc.is_empty() {
        request = request.query(&[("gameVersion", mc)]);
    }
    let projects: Vec<Project> = get(request)?;
    Ok(projects.into_iter().map(|p| p.into_hit(class)).collect())
}
impl Project {
    fn into_hit(self, class: &str) -> Hit {
        Hit {
            id: self.id.to_string(),
            title: self.name,
            description: self.summary,
            icon_url: self
                .logo
                .and_then(|l| l.thumbnail_url.filter(|u| !u.trim().is_empty()).or(l.url)),
            reference_version: None,
            reference_size: self
                .latest_files
                .into_iter()
                .filter(|f| {
                    f.file_name.to_ascii_lowercase().ends_with(if class == "6" {
                        ".jar"
                    } else {
                        ".zip"
                    })
                })
                .max_by(|a, b| a.file_date.cmp(&b.file_date))
                .and_then(|f| f.file_length),
        }
    }
}
pub fn releases(
    kind: Kind,
    loader: Option<Loader>,
    mc: &str,
    id: &str,
    include_manual: bool,
) -> Result<Vec<Release>> {
    let (http, api) = api()?;
    let id = id.parse::<u64>()?;
    let mut releases = vec![];
    let mut index = 0;
    loop {
        let mut request = http
            .get(format!("{api}/mods/{id}/files"))
            .query(&[("pageSize", "50"), ("index", &index.to_string())]);
        if let Some(loader) = loader {
            request = request.query(&[("modLoaderType", loader_id(loader)?)]);
        }
        if !mc.is_empty() {
            request = request.query(&[("gameVersion", mc)]);
        }
        let files: Vec<File> = get(request)?;
        let count = files.len();
        for f in files {
            if f.mod_id == id
                && f.file_name
                    .to_ascii_lowercase()
                    .ends_with(kind.extension(Source::CurseForge))
                && (include_manual || f.url().is_ok())
            {
                releases.push(Release {
                    source: crate::market::Source::CurseForge,
                    project: id.to_string(),
                    id: f.id.to_string(),
                    name: f.display_name.clone(),
                    url: f.url().unwrap_or_default(),
                    sha1: f.sha1()?,
                    filename: f.file_name,
                    size: f.file_length,
                    metadata: crate::market::status::Metadata {
                        version: f.display_name,
                        loaders: f
                            .game_versions
                            .iter()
                            .filter(|v| {
                                matches!(
                                    v.to_ascii_lowercase().as_str(),
                                    "fabric" | "forge" | "neoforge" | "quilt"
                                )
                            })
                            .cloned()
                            .collect(),
                        games: f.game_versions,
                        published: f.file_date.and_then(|s| s.parse().ok()),
                    },
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
impl Requirement {
    pub(super) fn required(&self) -> bool {
        self.required
    }
}
pub(super) fn resolve(requirements: &[Requirement], required: bool) -> Result<Vec<PackFile>> {
    let requirements: Vec<_> = requirements
        .iter()
        .filter(|r| r.required == required)
        .collect();
    if requirements.is_empty() {
        return Ok(vec![]);
    }
    let (http, api) = api()?;
    let mut resolved = vec![];
    for chunk in requirements.chunks(50) {
        let ids: Vec<_> = chunk.iter().map(|r| r.file).collect();
        let projects: Vec<_> = chunk.iter().map(|r| r.project).collect();
        let files: Vec<File> = get(http
            .post(format!("{api}/mods/files"))
            .json(&serde_json::json!({"fileIds":ids})))?;
        let mods: Vec<Project> = get(http
            .post(format!("{api}/mods"))
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
            let (folder, category) = match project.class_id {
                Some(6) => ("mods", "mc-mods"),
                Some(12) => ("resourcepacks", "texture-packs"),
                Some(6552) => ("shaderpacks", "shaders"),
                _ if !r.required => continue,
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
                size: file.file_length,
                downloads: file.download_url.clone().into_iter().collect(),
                automatic: None,
                hashes: HashMap::from([("sha1".into(), file.sha1()?)]),
                env: if r.required {
                    HashMap::new()
                } else {
                    HashMap::from([("client".into(), "optional".into())])
                },
                website: Some(format!(
                    "https://www.curseforge.com/minecraft/{category}/{}/files/{}",
                    project.slug, file.id
                )),
            });
        }
    }
    Ok(resolved)
}

pub struct Jar {
    pub project: u64,
    pub id: u64,
    pub name: String,
    pub hash: String,
    pub url: String,
    pub required: Vec<u64>,
}
pub fn latest_jar(project: u64, loader: Loader, mc: &str) -> Result<Jar> {
    let (http, api) = api()?;
    let files: Vec<File> = get(http.get(format!("{api}/mods/{project}/files")).query(&[
        ("gameVersion", mc),
        ("modLoaderType", loader_id(loader)?),
        ("pageSize", "50"),
    ]))?;
    let f = files
        .into_iter()
        .find(|f| f.mod_id == project && f.file_name.to_ascii_lowercase().ends_with(".jar"))
        .context("没有兼容的 CurseForge 模组版本")?;
    as_jar(project, f)
}

pub fn pinned_jar(project: u64, id: u64, loader: Loader, mc: &str) -> Result<Jar> {
    let (http, api) = api()?;
    let f: File = get(http.get(format!("{api}/mods/{project}/files/{id}")))?;
    ensure!(
        f.id == id
            && f.game_versions.iter().any(|v| v == mc)
            && f.game_versions
                .iter()
                .any(|v| v.eq_ignore_ascii_case(loader.label())),
        "所选模组与目标实例不兼容"
    );
    as_jar(project, f)
}

fn as_jar(project: u64, f: File) -> Result<Jar> {
    ensure!(
        f.mod_id == project && f.file_name.to_ascii_lowercase().ends_with(".jar"),
        "模组文件与项目不匹配"
    );
    Ok(Jar {
        project,
        id: f.id,
        hash: f.sha1()?,
        url: f.url()?,
        name: f.file_name,
        required: f
            .dependencies
            .into_iter()
            .filter(|d| d.relation_type == 3)
            .map(|d| d.mod_id)
            .collect(),
    })
}

pub fn fingerprint(bytes: &[u8]) -> u32 {
    const M: u32 = 0x5bd1e995;
    let data: Vec<_> = bytes
        .iter()
        .copied()
        .filter(|b| !matches!(b, 9 | 10 | 13 | 32))
        .collect();
    let mut h = 1 ^ (data.len() as u32);
    let (chunks, tail) = data.as_chunks::<4>();
    for c in chunks {
        let k = u32::from_le_bytes(*c).wrapping_mul(M);
        h = h.wrapping_mul(M) ^ ((k ^ (k >> 24)).wrapping_mul(M));
    }
    if !tail.is_empty() {
        for (i, c) in tail.iter().enumerate() {
            h ^= (*c as u32) << (8 * i);
        }
        h = h.wrapping_mul(M);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^ (h >> 15)
}
pub fn match_jars(values: &[u32]) -> Result<HashMap<String, (u64, u64)>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Matches {
        exact_matches: Vec<Matched>,
    }
    #[derive(Deserialize)]
    struct Matched {
        file: File,
    }
    let (http, api) = api()?;
    let mut result = HashMap::new();
    for batch in values.chunks(100) {
        let matches: Matches = get(http
            .post(format!("{api}/fingerprints/432"))
            .json(&serde_json::json!({"fingerprints":batch})))?;
        for m in matches.exact_matches {
            result.insert(m.file.sha1()?, (m.file.mod_id, m.file.id));
        }
    }
    Ok(result)
}
