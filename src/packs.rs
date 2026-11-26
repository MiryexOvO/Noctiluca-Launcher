pub(crate) mod jobs;
mod supply;
pub(crate) use supply::is_pack_archive;

use anyhow::{Context, Result, bail, ensure};
use mc_launcher_core::net::download::{Checksum, DownloadTask};
use serde::Deserialize;
use sha2::{Digest, Sha512};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use url::Url;
use zip::ZipArchive;

use crate::{
    download::Downloads,
    library::{Instance, Loader, safe_id},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    format_version: u32,
    game: String,
    name: String,
    dependencies: HashMap<String, String>,
    files: Vec<PackFile>,
}
#[derive(Deserialize, serde::Serialize)]
pub(crate) struct PackFile {
    pub(crate) path: String,
    #[serde(rename = "fileSize")]
    pub(crate) size: Option<u64>,
    pub(crate) hashes: HashMap<String, String>,
    pub(crate) downloads: Vec<String>,
    #[serde(skip)]
    pub(crate) website: Option<String>,
    #[serde(skip)]
    pub(crate) automatic: Option<Url>,
    #[serde(default)]
    pub(crate) env: HashMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseManifest {
    manifest_type: String,
    manifest_version: u32,
    name: String,
    minecraft: CurseGame,
    files: Vec<crate::curse::Requirement>,
    overrides: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseGame {
    version: String,
    mod_loaders: Vec<CurseLoader>,
}
#[derive(Deserialize)]
struct CurseLoader {
    id: String,
    #[serde(default)]
    primary: bool,
}

struct Manifest {
    index: Index,
    prefixes: Vec<String>,
    curse: Option<Vec<crate::curse::Requirement>>,
}

fn read_index(zip: &mut ZipArchive<File>) -> Result<Manifest> {
    if zip.index_for_name("modrinth.index.json").is_some() {
        let manifest = zip.by_name("modrinth.index.json")?;
        ensure!(manifest.size() <= 16 * 1024 * 1024, "整合包清单过大");
        return Ok(Manifest {
            index: serde_json::from_reader(manifest)?,
            prefixes: vec!["overrides/".into(), "client-overrides/".into()],
            curse: None,
        });
    }
    let mmc = zip
        .file_names()
        .find(|n| *n == "mmc-pack.json" || n.ends_with("/mmc-pack.json"))
        .map(str::to_owned);
    if let Some(name) = mmc {
        let prefix = name.strip_suffix("mmc-pack.json").unwrap();
        if !prefix.is_empty() {
            relative(prefix.trim_end_matches('/'))?;
        }
        ensure!(
            !zip.file_names()
                .any(|n| n != name && (n == "mmc-pack.json" || n.ends_with("/mmc-pack.json"))),
            "压缩包包含多个实例"
        );
        let entry = zip.by_name(&name)?;
        ensure!(entry.size() <= 1024 * 1024, "MultiMC 清单过大");
        let manifest: serde_json::Value = serde_json::from_reader(entry)?;
        ensure!(manifest["formatVersion"] == 1, "未知 MultiMC 格式");
        let mut deps = HashMap::new();
        for c in manifest["components"]
            .as_array()
            .context("缺少 MultiMC 组件")?
        {
            let uid = c["uid"].as_str().context("缺少组件 ID")?;
            let k = match uid {
                "net.minecraft" => "minecraft",
                "net.minecraftforge" => "forge",
                "net.neoforged" => "neoforge",
                "net.fabricmc.fabric-loader" => "fabric-loader",
                "org.quiltmc.quilt-loader" => "quilt-loader",
                "org.lwjgl" | "org.lwjgl3" | "net.fabricmc.intermediary" | "org.quiltmc.hashed" => {
                    continue;
                }
                _ => bail!("暂不支持 MultiMC 组件 {uid}"),
            };
            ensure!(
                deps.insert(
                    k.into(),
                    c["version"].as_str().context("缺少组件版本")?.to_owned()
                )
                .is_none(),
                "重复的 MultiMC 组件"
            );
        }
        ensure!(
            !zip.file_names()
                .any(|n| n.starts_with(&format!("{prefix}patches/")) && n.ends_with(".json")),
            "此实例包含自定义组件补丁"
        );
        let mut title = prefix
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_owned();
        if let Ok(cfg) = zip.by_name(&format!("{prefix}instance.cfg")) {
            let mut txt = String::new();
            cfg.take(256 * 1024).read_to_string(&mut txt)?;
            if let Some(v) = txt.lines().find_map(|l| l.strip_prefix("name=")) {
                title = v.trim().to_owned();
            }
        }
        if title.is_empty() {
            title = "MultiMC 实例".into();
        }
        let folders = [".minecraft/", "minecraft/"].map(|v| format!("{prefix}{v}"));
        let prefixes = folders
            .into_iter()
            .filter(|p| zip.file_names().any(|n| n.starts_with(p)))
            .collect::<Vec<_>>();
        ensure!(prefixes.len() <= 1, "MultiMC 实例包含两个游戏目录");
        return Ok(Manifest {
            index: Index {
                format_version: 1,
                game: "minecraft".into(),
                name: title,
                dependencies: deps,
                files: vec![],
            },
            prefixes,
            curse: None,
        });
    }
    let manifest = zip
        .by_name("manifest.json")
        .context("未找到 Modrinth、CurseForge 或 MultiMC 清单")?;
    ensure!(manifest.size() <= 16 * 1024 * 1024, "整合包清单过大");
    let m: CurseManifest = serde_json::from_reader(manifest)?;
    ensure!(
        m.manifest_type == "minecraftModpack" && m.manifest_version == 1,
        "不支持此 CurseForge 清单"
    );
    let mut deps = HashMap::from([("minecraft".into(), m.minecraft.version)]);
    if !m.minecraft.mod_loaders.is_empty() {
        let loader = m
            .minecraft
            .mod_loaders
            .iter()
            .find(|v| v.primary)
            .unwrap_or(&m.minecraft.mod_loaders[0]);
        let (kind, version) = loader.id.split_once('-').context("加载器未指定版本")?;
        let key = match kind {
            "fabric" => "fabric-loader",
            "quilt" => "quilt-loader",
            "neoforge" => "neoforge",
            "forge" => "forge",
            _ => bail!("不支持的加载器：{kind}"),
        };
        deps.insert(key.into(), version.into());
    }
    let prefix = m.overrides.trim_end_matches('/');
    let prefixes = if prefix.is_empty() {
        vec![]
    } else {
        relative(prefix)?;
        vec![format!("{prefix}/")]
    };
    Ok(Manifest {
        index: Index {
            format_version: 1,
            game: "minecraft".into(),
            name: m.name,
            dependencies: deps,
            files: vec![],
        },
        prefixes,
        curse: Some(m.files),
    })
}

pub(crate) fn relative(name: &str) -> Result<PathBuf> {
    let mut path = PathBuf::new();
    for part in name.split('/') {
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let reserved = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || ["COM", "LPT"].iter().any(|p| {
            stem.strip_prefix(p)
                .is_some_and(|digit| "123456789¹²³".contains(digit) && digit.chars().count() == 1)
        });
        ensure!(
            !part.is_empty()
                && !part.ends_with(['.', ' '])
                && !reserved
                && !part.contains(['\\', ':', '<', '>', '"', '|', '?', '*'])
                && !part.chars().any(char::is_control),
            "整合包包含无效路径：{name}"
        );
        path.push(part);
    }
    Ok(path)
}

fn download_host(url: &Url, curse: bool) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.host_str().is_some_and(|host| {
            matches!(
                host,
                "cdn.modrinth.com" | "github.com" | "gitlab.com" | "raw.githubusercontent.com"
            ) || host.ends_with(".githubusercontent.com")
                || (curse && host.ends_with(".forgecdn.net"))
        })
}

pub struct Preview {
    pub instance: Instance,
    pub files: Vec<PackFile>,
    pub curse: bool,
    pub archive_sha1: Option<String>,
    pub format: &'static str,
    pub upstream: Option<crate::content::Upstream>,
    pub target: Option<Instance>,
    pub metadata_error: Option<String>,
    pub optional_error: Option<String>,
    pub required_count: usize,
    pub optional_count: usize,
    archive: File,
    prefixes: Vec<String>,
    cache: PathBuf,
    _scratch: Option<tempfile::TempDir>,
    pub job: Option<std::sync::Arc<jobs::Job>>,
    overrides: std::collections::BTreeMap<String, String>,
    cached_sizes: std::sync::Mutex<Vec<Option<u64>>>,
    pub active_files: std::sync::Mutex<std::collections::BTreeSet<String>>,
}

impl PackFile {
    pub fn optional(&self) -> bool {
        self.env.get("client").is_some_and(|v| v == "optional")
    }

    pub fn source(&self, curse: bool) -> Option<Url> {
        self.downloads
            .iter()
            .filter_map(|s| Url::parse(s).ok())
            .find(|u| download_host(u, curse))
    }

    pub fn website(&self) -> Option<String> {
        if let Some(url) = &self.website {
            return Some(url.clone());
        }
        for value in &self.downloads {
            let Ok(url) = Url::parse(value) else { continue };
            if url.scheme() == "https"
                && url.host_str() == Some("cdn.modrinth.com")
                && let Some(segments) = url.path_segments()
                && let ["data", id, ..] = segments.collect::<Vec<_>>().as_slice()
                && !id.is_empty()
                && id.bytes().all(|c| c.is_ascii_alphanumeric())
            {
                return Some(format!("https://modrinth.com/project/{id}"));
            }
        }
        self.source(false).map(Into::into)
    }
}

fn resolve_curse(
    job: Option<&jobs::Job>,
    requirements: &[crate::curse::Requirement],
    required: bool,
) -> Result<Vec<PackFile>> {
    let Some(job) = job else {
        return crate::curse::resolve(requirements, required);
    };
    let path = job.dir.join(if required {
        "curse-required.json"
    } else {
        "curse-optional.json"
    });
    if let Ok(input) = File::open(&path)
        && let Ok(rows) = serde_json::from_reader::<_, Vec<(PackFile, Option<String>)>>(input)
    {
        return Ok(rows
            .into_iter()
            .map(|(mut file, site)| {
                file.website = site.filter(|s| {
                    Url::parse(s).is_ok_and(|u| {
                        u.scheme() == "https" && u.host_str() == Some("www.curseforge.com")
                    })
                });
                file
            })
            .collect());
    }
    let files = crate::curse::resolve(requirements, required)?;
    let rows = files.iter().map(|f| (f, &f.website)).collect::<Vec<_>>();
    crate::library::atomic_write(&path, &serde_json::to_vec(&rows)?)?;
    Ok(files)
}

pub fn preview_job(
    root: &Path,
    path: &Path,
    job: Option<std::sync::Arc<jobs::Job>>,
    cancel: &AtomicBool,
) -> Result<Preview> {
    let mut zip = ZipArchive::new(File::open(path).context("无法打开所选整合包")?)
        .context("无法读取整合包压缩文件")?;
    let multi = zip
        .file_names()
        .any(|n| n == "mmc-pack.json" || n.ends_with("/mmc-pack.json"))
        && zip.index_for_name("modrinth.index.json").is_none();
    let Manifest {
        mut index,
        prefixes,
        curse,
    } = read_index(&mut zip)?;
    let mut metadata_error = None;
    let mut optional_error = None;
    let mut required_count = 0;
    let mut optional_count = 0;
    if let Some(ref requirements) = curse {
        required_count = requirements.iter().filter(|f| f.required()).count();
        optional_count = requirements.len() - required_count;
        match resolve_curse(job.as_deref(), requirements, true) {
            Ok(files) => index.files = files,
            Err(e) => metadata_error = Some(format!("{e:#}")),
        }
        if metadata_error.is_none() {
            match resolve_curse(job.as_deref(), requirements, false) {
                Ok(files) => index.files.extend(files),
                Err(e) => optional_error = Some(format!("{e:#}")),
            }
        }
    }
    ensure!(
        index.format_version == 1 && index.game == "minecraft",
        "不支持此整合包格式"
    );
    let mut dependencies = index.dependencies;
    let mc = dependencies
        .remove("minecraft")
        .context("缺少 Minecraft 版本")?;
    let mut instance = Instance::new(index.name, mc, Loader::Vanilla)?;
    for (key, v) in dependencies {
        ensure!(
            instance.loader == Loader::Vanilla && safe_id(&v),
            "整合包要求多个加载器或版本号无效"
        );
        instance.loader = match key.as_str() {
            "fabric-loader" => Loader::Fabric,
            "quilt-loader" => Loader::Quilt,
            "forge" => Loader::Forge,
            "neoforge" => Loader::NeoForge,
            _ => bail!("不支持整合包依赖 {key}"),
        };
        instance.loader_version = Some(v);
    }
    let mut names = HashSet::new();
    let mut files = Vec::new();
    for mut file in index.files {
        if file.env.get("client").is_some_and(|v| v == "unsupported") {
            continue;
        }
        relative(&file.path)?;
        ensure!(
            names.insert(file.path.to_lowercase()),
            "清单包含重复文件：{}",
            file.path
        );
        for (key, len) in [("sha1", 40), ("sha512", 128)] {
            if key == "sha512" && curse.is_some() {
                continue;
            }
            ensure!(
                file.hashes
                    .get(key)
                    .is_some_and(|s| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())),
                "{} 缺少有效的 {key} 校验值",
                file.path
            );
        }
        file.automatic = file.source(curse.is_some());
        files.push(file);
    }
    if metadata_error.is_none() && optional_error.is_none() {
        optional_count = files.iter().filter(|f| f.optional()).count();
        required_count = files.len() - optional_count;
    }
    let scratch = if job.is_none() {
        Some(tempfile::tempdir_in(root)?)
    } else {
        None
    };
    let cache = if let Some(j) = &job {
        j.dir.join("files")
    } else {
        scratch.as_ref().unwrap().path().to_owned()
    };
    fs::create_dir_all(&cache)?;
    let mut overrides = std::collections::BTreeMap::new();
    if job.as_ref().is_some_and(|j| j.record().target.is_some()) {
        for prefix in &prefixes {
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i)?;
                let Some(rel) = entry.name().strip_prefix(prefix).map(str::to_owned) else {
                    continue;
                };
                if entry.is_dir() || rel.is_empty() {
                    continue;
                }
                relative(&rel)?;
                ensure!(!entry.is_symlink(), "整合包包含符号链接");
                if !crate::content::review::relevant(&rel) {
                    continue;
                }
                let mut digest = sha1::Sha1::new();
                let mut buf = [0u8; 64 * 1024];
                loop {
                    ensure!(
                        !cancel.load(std::sync::atomic::Ordering::Relaxed),
                        "已暂停任务"
                    );
                    let n = entry.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    digest.update(&buf[..n]);
                }
                overrides.insert(rel, hex::encode(digest.finalize()));
            }
        }
    }
    if let Some(j) = &job {
        instance.id = j.record().id;
    }
    let preview = Preview {
        cached_sizes: std::sync::Mutex::new(vec![None; files.len()]),
        active_files: Default::default(),
        instance,
        files,
        curse: curse.is_some(),
        archive_sha1: if !multi && curse.is_none() {
            Some(mc_launcher_core::io::hash::sha1_file(path)?)
        } else {
            None
        },
        format: if multi {
            "MultiMC"
        } else if curse.is_some() {
            "CurseForge"
        } else {
            "Modrinth"
        },
        upstream: None,
        target: None,
        metadata_error,
        optional_error,
        required_count,
        optional_count,
        archive: zip.into_inner(),
        prefixes,
        cache,
        _scratch: scratch,
        job,
        overrides,
    };
    preview.restore_cache(cancel)?;
    Ok(preview)
}

#[derive(Default)]
pub struct DownloadEstimate {
    pub cached_count: usize,
    pub cached_bytes: u64,
    pub pending_count: usize,
    pub pending_bytes: u64,
    pub unknown_count: usize,
}

impl Preview {
    pub fn estimate(&self, optional: &HashSet<usize>) -> DownloadEstimate {
        let cached = self.cached_sizes.lock().unwrap();
        let mut result = DownloadEstimate::default();
        if self.metadata_error.is_some() {
            result.pending_count = self.required_count;
            result.unknown_count = self.required_count;
            return result;
        }
        for (i, file) in self.files.iter().enumerate() {
            if file.optional() && !optional.contains(&i) {
                continue;
            }
            if let Some(bytes) = cached[i] {
                result.cached_bytes = result.cached_bytes.saturating_add(bytes);
                result.cached_count += 1;
            } else {
                result.pending_count += 1;
                if let Some(bytes) = file.size {
                    result.pending_bytes = result.pending_bytes.saturating_add(bytes);
                } else {
                    result.unknown_count += 1;
                }
            }
        }
        result
    }

    fn cache_path(&self, index: usize) -> PathBuf {
        let file = &self.files[index];
        self.cache.join(format!(
            "{}-{}",
            file.hashes["sha1"].to_ascii_lowercase(),
            hex::encode(sha1::Sha1::digest(file.path.as_bytes()))
        ))
    }

    fn restore_cache(&self, cancel: &AtomicBool) -> Result<()> {
        for (i, f) in self.files.iter().enumerate() {
            ensure!(
                !cancel.load(std::sync::atomic::Ordering::Relaxed),
                "已暂停任务"
            );
            let path = self.cache_path(i);
            if !path.try_exists()? {
                continue;
            }
            if mc_launcher_core::io::hash::sha1_file(&path)?.eq_ignore_ascii_case(&f.hashes["sha1"])
                && self.check_sha512(f, &path, cancel).is_ok()
            {
                self.cached_sizes.lock().unwrap()[i] = Some(fs::metadata(path)?.len());
            } else {
                ensure!(
                    !cancel.load(std::sync::atomic::Ordering::Relaxed),
                    "已暂停任务"
                );
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }
    pub fn cached(&self) -> HashSet<usize> {
        self.cached_sizes
            .lock()
            .unwrap()
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.is_some().then_some(i))
            .collect()
    }
    pub fn review(
        &self,
        root: &Path,
        optional: &HashSet<usize>,
    ) -> Result<crate::content::review::Review> {
        let item = self.target.as_ref().context("请选择要更新的实例")?;
        ensure!(item.external.is_none(), "原地更新仅支持启动器管理的实例");
        let old = crate::content::PackState::read(&item.path(root))?;
        let mut files = std::collections::BTreeMap::new();
        for (i, file) in self.files.iter().enumerate() {
            if !file.optional() || optional.contains(&i) {
                files.insert(
                    file.path.to_lowercase(),
                    (file.path.clone(), file.hashes["sha1"].to_ascii_lowercase()),
                );
            }
        }
        for (rel, hash) in &self.overrides {
            files
                .entry(rel.to_lowercase())
                .or_insert_with(|| (rel.clone(), String::new()))
                .1 = hash.clone();
        }
        let next = files.into_values().collect();
        crate::content::review::Review::build(&item.game_dir(root)?, old.as_ref(), &next)
    }
    pub fn retry_file(
        &self,
        index: usize,
        cancel: &AtomicBool,
        report: &(dyn Fn(String) + Sync),
    ) -> Result<()> {
        let file = self.files.get(index).context("文件已变化")?;
        ensure!(file.automatic.is_some(), "请手动补齐文件");
        self.cache_files(&self.downloads(cancel, report)?, &[(index, file)])
    }
    fn check_sha512(&self, file: &PackFile, path: &Path, cancel: &AtomicBool) -> Result<()> {
        if let Some(expected) = file.hashes.get("sha512") {
            let mut hash = Sha512::new();
            let mut input = File::open(path)?;
            let mut buffer = [0; 64 * 1024];
            loop {
                ensure!(
                    !cancel.load(std::sync::atomic::Ordering::Relaxed),
                    "已取消校验"
                );
                let n = input.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
            }
            ensure!(
                hex::encode(hash.finalize()).eq_ignore_ascii_case(expected),
                "{} 与清单 SHA-512 不符",
                file.path
            );
        }
        Ok(())
    }

    fn downloads<'a>(
        &self,
        cancel: &'a AtomicBool,
        report: &'a (dyn Fn(String) + Sync),
    ) -> Result<Downloads<'a>> {
        let curse = self.curse;
        let mut downloads = Downloads::new(cancel, report)?;
        downloads.http = crate::download::client_builder()
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if download_host(attempt.url(), curse) && attempt.previous().len() < 8 {
                    attempt.follow()
                } else {
                    attempt.error("整合包下载跳转到不受支持的地址")
                }
            }))
            .build()?;
        Ok(downloads)
    }

    fn cache_files(&self, downloads: &Downloads, selected: &[(usize, &PackFile)]) -> Result<()> {
        let mut tasks = Vec::new();
        for &(i, file) in selected {
            let destination = self.cache_path(i);
            if let Some(url) = &file.automatic {
                tasks.push(DownloadTask {
                    url: url.to_string(),
                    destination,
                    checksum: Some(Checksum::Sha1(file.hashes["sha1"].clone())),
                    label: file.path.clone(),
                });
            } else {
                ensure!(destination.is_file(), "请补齐文件：{}", file.path);
            }
        }
        let indices: HashMap<_, _> = selected
            .iter()
            .map(|(i, f)| (f.path.as_str(), *i))
            .collect();
        let result = downloads.batch_with(
            &self.cache,
            "整合包文件",
            tasks,
            |task| {
                let mut active = self.active_files.lock().unwrap();
                if active.is_empty() {
                    (downloads.report)(format!("正在获取 {}", task.label));
                }
                active.insert(task.label.clone());
            },
            |task| {
                self.active_files.lock().unwrap().remove(&task.label);
                let i = indices[task.label.as_str()];
                self.check_sha512(&self.files[i], &task.destination, downloads.cancel)?;
                self.cached_sizes.lock().unwrap()[i] = Some(fs::metadata(&task.destination)?.len());
                Ok(())
            },
        );
        self.active_files.lock().unwrap().clear();
        result
    }

    pub fn prefetch(&self, cancel: &AtomicBool, report: &(dyn Fn(String) + Sync)) -> Result<()> {
        let downloads = self.downloads(cancel, report)?;
        let required: Vec<_> = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.optional() && f.automatic.is_some())
            .collect();
        self.cache_files(&downloads, &required)
    }

    pub fn install_prepared(
        &self,
        root: &Path,
        optional: &HashSet<usize>,
        cancel: &AtomicBool,
        report: impl Fn(String) + Sync,
        pin: Option<&[u8]>,
        reviewed: Option<(
            &crate::content::review::Review,
            &crate::content::review::Choices,
        )>,
    ) -> Result<Instance> {
        if let Some(error) = &self.metadata_error {
            bail!("{error}");
        }
        let instance = &self.instance;
        let downloads = self.downloads(cancel, &report)?;
        let selected: Vec<_> = self
            .files
            .iter()
            .enumerate()
            .filter(|(i, f)| !f.optional() || optional.contains(i))
            .collect();
        self.cache_files(&downloads, &selected)?;
        let temp = tempfile::tempdir_in(root)?;
        let game = temp.path().join("game");
        fs::create_dir_all(&game)?;
        for &(i, file) in &selected {
            downloads.check()?;
            let cached = self.cache_path(i);
            if file.automatic.is_none() {
                ensure!(
                    mc_launcher_core::io::hash::sha1_file(&cached)?
                        .eq_ignore_ascii_case(&file.hashes["sha1"]),
                    "手动文件校验失败：{}",
                    file.path
                );
            }

            self.check_sha512(file, &cached, cancel)
                .with_context(|| crate::download::FileFailure::new("整合包校验", &file.path))?;
            let dest = game.join(relative(&file.path)?);
            fs::create_dir_all(dest.parent().context("无效文件路径")?)?;
            fs::copy(cached, dest)?;
        }
        report("应用整合包配置".into());
        let mut zip = ZipArchive::new(self.archive.try_clone()?)?;
        for prefix in &self.prefixes {
            for idx in 0..zip.len() {
                downloads.check()?;
                let mut entry = zip.by_index(idx)?;
                let Some(rel) = entry.name().strip_prefix(prefix.as_str()) else {
                    continue;
                };
                let rel = rel.trim_end_matches('/');
                if rel.is_empty() {
                    continue;
                }
                let dest = game.join(relative(rel)?);
                ensure!(!entry.is_symlink(), "整合包包含符号链接");
                if entry.is_dir() {
                    fs::create_dir_all(dest)?;
                    continue;
                }
                fs::create_dir_all(dest.parent().context("无效文件夹")?)?;
                io::copy(&mut entry, &mut File::create(dest)?)?;
            }
        }
        downloads.check()?;
        if let Some(icon) = crate::instance_icon::from_archive(&mut zip) {
            fs::write(temp.path().join(crate::instance_icon::FILENAME), icon)?;
        }
        let mut state =
            crate::content::PackState::from_game(&game, self.upstream.clone(), &instance.name)?;
        state.archive_sha1 = self.archive_sha1.clone();
        crate::library::atomic_write(
            &temp.path().join("pack-state.json"),
            &serde_json::to_vec(&state)?,
        )?;
        if let Some(pin) = pin {
            fs::write(temp.path().join("loader.json"), pin)?;
        }
        if let Some(target) = &self.target {
            let empty = Default::default();
            let merge = crate::content::Merge {
                review: reviewed.map(|(r, _)| r),
                choices: reviewed.map_or(&empty, |(_, c)| c),
                task: self.job.as_ref().map(|_| instance.id.as_str()),
            };
            return crate::content::apply_pack(
                root,
                target,
                instance,
                temp.path(),
                &state,
                merge,
                cancel,
            );
        }
        if self.job.is_some() {
            fs::write(temp.path().join("pack-task-id"), &instance.id)?;
        }
        fs::write(
            temp.path().join("instance.json"),
            serde_json::to_vec_pretty(instance)?,
        )?;
        downloads.check()?;
        fs::rename(temp.path(), instance.path(root))?;
        Ok(instance.clone())
    }
}
