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

fn read_index(zip: &mut ZipArchive<File>) -> Result<(Index, Vec<String>, bool)> {
    if zip.index_for_name("modrinth.index.json").is_some() {
        let manifest = zip.by_name("modrinth.index.json")?;
        ensure!(manifest.size() <= 16 * 1024 * 1024, "整合包清单过大");
        return Ok((
            serde_json::from_reader(manifest)?,
            vec!["overrides/".into(), "client-overrides/".into()],
            false,
        ));
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
    let files = crate::curse::resolve(m.files)?;
    Ok((
        Index {
            format_version: 1,
            game: "minecraft".into(),
            name: m.name,
            dependencies: deps,
            files,
        },
        prefixes,
        true,
    ))
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

pub fn import(
    root: &Path,
    path: &Path,
    cancel: &AtomicBool,
    report: impl Fn(String) + Sync,
) -> Result<Instance> {
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let (index, prefixes, curse) = read_index(&mut zip)?;
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
    let temp = tempfile::tempdir_in(root)?;
    let game = temp.path().join("game");
    fs::create_dir_all(&game)?;
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
    let mut tasks = Vec::new();
    let mut hashes = Vec::new();
    let mut names = HashSet::new();
    for file in index.files {
        if matches!(
            file.env.get("client").map(String::as_str),
            Some("optional" | "unsupported")
        ) {
            continue;
        }
        let dest = game.join(relative(&file.path)?);
        ensure!(names.insert(file.path.to_lowercase()), "清单包含重复文件");
        let url = file
            .downloads
            .iter()
            .filter_map(|s| Url::parse(s).ok())
            .find(|url| download_host(url, curse))
            .context("没有可用的整合包下载来源")?;
        let hash = file
            .hashes
            .get("sha1")
            .context("整合包文件缺少 SHA-1")?
            .clone();
        if !curse {
            hashes.push((
                dest.clone(),
                file.hashes
                    .get("sha512")
                    .context("整合包文件缺少 SHA-512")?
                    .clone(),
            ));
        }
        tasks.push(DownloadTask {
            url: url.to_string(),
            destination: dest,
            checksum: Some(Checksum::Sha1(hash)),
            label: file.path,
        });
    }
    downloads.batch(&game, "整合包文件", tasks)?;
    for (path, expected) in hashes {
        downloads.check()?;
        let mut hash = Sha512::new();
        let mut input = File::open(path)?;
        let mut buffer = [0; 64 * 1024];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        let actual: String = hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        ensure!(
            actual.eq_ignore_ascii_case(&expected),
            "整合包 SHA-512 校验失败"
        );
    }
    report("应用整合包配置".into());
    for prefix in prefixes {
        for idx in 0..zip.len() {
            downloads.check()?;
            let mut entry = zip.by_index(idx)?;
            let Some(rel) = entry.name().strip_prefix(&prefix) else {
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
        serde_json::to_vec_pretty(&instance)?,
    )?;
    fs::rename(temp.path(), instance.path(root))?;
    Ok(instance)
}
