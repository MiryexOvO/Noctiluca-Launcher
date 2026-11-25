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
#[derive(Deserialize)]
pub(crate) struct PackFile {
    pub(crate) path: String,
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
    let manifest = zip
        .by_name("manifest.json")
        .context("未找到 Modrinth 或 CurseForge 整合包清单")?;
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

fn relative(name: &str) -> Result<PathBuf> {
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
    pub metadata_error: Option<String>,
    pub optional_error: Option<String>,
    pub required_count: usize,
    pub optional_count: usize,
    archive: File,
    prefixes: Vec<String>,
    cache: tempfile::TempDir,
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

pub fn preview(root: &Path, path: &Path) -> Result<Preview> {
    let mut zip = ZipArchive::new(File::open(path).context("无法打开所选整合包")?)
        .context("无法读取整合包压缩文件")?;
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
        match crate::curse::resolve(requirements, true) {
            Ok(files) => index.files = files,
            Err(e) => metadata_error = Some(format!("{e:#}")),
        }
        if metadata_error.is_none() {
            match crate::curse::resolve(requirements, false) {
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
    Ok(Preview {
        instance,
        files,
        curse: curse.is_some(),
        metadata_error,
        optional_error,
        required_count,
        optional_count,
        archive: zip.into_inner(),
        prefixes,
        cache: tempfile::tempdir_in(root)?,
    })
}

impl Preview {
    fn cache_path(&self, index: usize) -> PathBuf {
        self.cache.path().join(index.to_string())
    }

    pub fn supply(&self, index: usize, source: &Path, cancel: &AtomicBool) -> Result<()> {
        let file = self.files.get(index).context("清单文件不存在")?;
        let mut input = File::open(source).context("无法读取所选文件")?;
        let mut copy = tempfile::NamedTempFile::new_in(self.cache.path())?;
        io::copy(&mut input, &mut copy)?;
        ensure!(
            mc_launcher_core::io::hash::sha1_file(copy.path())?
                .eq_ignore_ascii_case(&file.hashes["sha1"]),
            "{} 与清单 SHA-1 不符，请选择指定版本",
            file.path
        );
        self.check_sha512(file, copy.path(), cancel)?;
        ensure!(
            !cancel.load(std::sync::atomic::Ordering::Relaxed),
            "已取消校验"
        );
        copy.persist(self.cache_path(index))?;
        Ok(())
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

    pub fn install(
        &self,
        root: &Path,
        optional: &HashSet<usize>,
        cancel: &AtomicBool,
        report: impl Fn(String) + Sync,
    ) -> Result<Instance> {
        if let Some(error) = &self.metadata_error {
            bail!("{error}");
        }
        let instance = &self.instance;
        let curse = self.curse;
        let mut downloads = Downloads::new(cancel, &report)?;
        downloads.http = crate::download::client_builder()
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if download_host(attempt.url(), curse) && attempt.previous().len() < 8 {
                    attempt.follow()
                } else {
                    attempt.error("整合包下载跳转到不受支持的地址")
                }
            }))
            .build()?;
        let selected: Vec<_> = self
            .files
            .iter()
            .enumerate()
            .filter(|(i, f)| !f.optional() || optional.contains(i))
            .collect();
        let mut tasks = Vec::new();
        for &(i, file) in &selected {
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
        downloads.batch(self.cache.path(), "整合包文件", tasks)?;
        let temp = tempfile::tempdir_in(root)?;
        let game = temp.path().join("game");
        fs::create_dir_all(&game)?;
        for &(i, file) in &selected {
            downloads.check()?;
            let cached = self.cache_path(i);
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
        fs::write(
            temp.path().join("instance.json"),
            serde_json::to_vec_pretty(instance)?,
        )?;
        downloads.check()?;
        fs::rename(temp.path(), instance.path(root))?;
        Ok(instance.clone())
    }
}
