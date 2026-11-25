use anyhow::{Context, Result, bail, ensure};
use mc_launcher_core::{
    io::hash::sha1_file,
    net::download::{Checksum, DownloadTask},
};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Component, Path},
    sync::atomic::AtomicBool,
};

use crate::{
    download::{Downloads, client},
    library::{Instance, Loader, safe_id},
};

const API: &str = "https://api.modrinth.com/v2";

#[derive(Clone, Deserialize)]
pub struct Hit {
    pub project_id: String,
    pub title: String,
}
#[derive(Deserialize)]
struct Search {
    hits: Vec<Hit>,
}
#[derive(Deserialize)]
struct Version {
    id: String,
    project_id: String,
    game_versions: Vec<String>,
    loaders: Vec<String>,
    dependencies: Vec<Dependency>,
    files: Vec<ModFile>,
}
#[derive(Deserialize)]
struct Dependency {
    version_id: Option<String>,
    project_id: Option<String>,
    dependency_type: String,
}
#[derive(Deserialize)]
struct ModFile {
    filename: String,
    url: String,
    primary: bool,
    hashes: HashMap<String, String>,
}

fn loader(instance: &Instance) -> Result<&'static str> {
    match instance.loader {
        Loader::Fabric => Ok("fabric"),
        Loader::Quilt => Ok("quilt"),
        Loader::Forge => Ok("forge"),
        Loader::NeoForge => Ok("neoforge"),
        Loader::Vanilla => bail!("请先选择带加载器的实例"),
    }
}

pub fn search(instance: &Instance, query: &str) -> Result<Vec<Hit>> {
    let facets = serde_json::json!([
        ["project_type:mod"],
        [format!("categories:{}", loader(instance)?)],
        [format!("versions:{}", instance.version)],
        ["client_side:required", "client_side:optional"]
    ])
    .to_string();
    Ok(client()?
        .get(format!("{API}/search"))
        .query(&[("query", query), ("facets", &facets), ("limit", "10")])
        .send()?
        .error_for_status()?
        .json::<Search>()?
        .hits)
}

fn jar_name(name: &str) -> bool {
    let path = Path::new(name);
    path.components().count() == 1
        && matches!(path.components().next(), Some(Component::Normal(_)))
        && !name.contains(['/', '\\', ':', '*', '?', '<', '>', '|', '"'])
        && !name.starts_with('.')
        && name.to_ascii_lowercase().ends_with(".jar")
}

pub fn installed(dir: &Path) -> Result<Vec<String>> {
    fs::create_dir_all(dir)?;
    let mut jars = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_file() && jar_name(name.strip_suffix(".disabled").unwrap_or(&name))
        {
            jars.push(name)
        }
    }
    jars.sort_by_key(|s| s.to_lowercase());
    Ok(jars)
}

pub fn toggle(root: &Path, item: &Instance, name: &str) -> Result<()> {
    let _lock = crate::library::lock_instance(root, &item.path(root), "实例正在使用")?;
    let dir = item.path(root).join("game/mods");
    let base = name.strip_suffix(".disabled").unwrap_or(name);
    ensure!(jar_name(base), "模组文件名无效");
    let next = if name == base {
        format!("{base}.disabled")
    } else {
        base.into()
    };
    ensure!(!dir.join(&next).exists(), "目标文件已经存在");
    fs::rename(dir.join(name), dir.join(next))?;
    Ok(())
}

pub fn install(
    root: &Path,
    item: &Instance,
    hit: &Hit,
    cancel: &AtomicBool,
    report: impl Fn(String) + Sync,
) -> Result<()> {
    let _lock = crate::library::lock_instance(root, &item.path(root), "实例正在使用")?;
    let downloads = Downloads::new(cancel, &report)?;
    let target = loader(item)?;
    let query = [
        (
            "game_versions",
            serde_json::json!([item.version]).to_string(),
        ),
        ("loaders", serde_json::json!([target]).to_string()),
    ];
    let mut queue = vec![(Some(hit.project_id.clone()), None::<String>)];
    let mut resolved = HashMap::new();
    let mut files = Vec::new();
    let mut names = HashSet::new();
    let mods = item.path(root).join("game/mods");
    fs::create_dir_all(&mods)?;
    let temp = tempfile::tempdir_in(&mods)?;
    report("解析模组及必需依赖".into());
    while let Some((project, exact)) = queue.pop() {
        downloads.check()?;
        if let Some(id) = project.as_ref().and_then(|id| resolved.get(id)) {
            ensure!(
                exact.as_ref().is_none_or(|wanted| wanted == id),
                "必需依赖的版本存在冲突"
            );
            continue;
        }
        let version: Version = if let Some(id) = exact {
            ensure!(safe_id(&id), "依赖版本 ID 无效");
            downloads
                .http
                .get(format!("{API}/version/{id}"))
                .send()?
                .error_for_status()?
                .json()?
        } else {
            let id = project.as_deref().context("依赖缺少项目 ID")?;
            ensure!(safe_id(id), "模组项目 ID 无效");
            downloads
                .http
                .get(format!("{API}/project/{id}/version"))
                .query(&query)
                .send()?
                .error_for_status()?
                .json::<Vec<Version>>()?
                .into_iter()
                .next()
                .with_context(|| format!("项目 {id} 没有匹配的版本"))?
        };
        ensure!(
            version.game_versions.contains(&item.version)
                && version.loaders.iter().any(|v| v == target),
            "依赖与当前实例不兼容"
        );
        if let Some(existing) = resolved.insert(version.project_id.clone(), version.id.clone()) {
            ensure!(existing == version.id, "项目要求的依赖版本冲突");
            continue;
        }
        let file = version
            .files
            .iter()
            .find(|f| f.primary)
            .or(version.files.first())
            .context("模组版本没有文件")?;
        ensure!(
            jar_name(&file.filename) && names.insert(file.filename.to_lowercase()),
            "模组文件名无效或重复"
        );
        let hash = file.hashes.get("sha1").context("缺少文件校验值")?;
        let destination = mods.join(&file.filename);
        ensure!(
            !mods.join(format!("{}.disabled", file.filename)).exists(),
            "请先启用同名模组 {}",
            file.filename
        );
        if destination.exists() {
            ensure!(
                sha1_file(&destination)? == *hash,
                "同名模组文件内容不同：{}",
                file.filename
            );
        } else {
            files.push(DownloadTask {
                url: file.url.clone(),
                destination: temp.path().join(&file.filename),
                checksum: Some(Checksum::Sha1(hash.clone())),
                label: file.filename.clone(),
            });
        }
        queue.extend(
            version
                .dependencies
                .into_iter()
                .filter(|d| d.dependency_type == "required")
                .map(|d| (d.project_id, d.version_id)),
        );
    }
    let publish: Vec<_> = files
        .iter()
        .map(|f| (f.destination.clone(), mods.join(&f.label)))
        .collect();
    downloads.batch(temp.path(), "下载模组", files)?;
    downloads.check()?;
    let mut created = Vec::new();
    for (from, to) in publish {
        if let Err(err) = fs::hard_link(from, &to) {
            for path in created {
                fs::remove_file(path)?;
            }
            return Err(err).context("无法将下载的模组放入实例");
        }
        created.push(to);
    }
    Ok(())
}
