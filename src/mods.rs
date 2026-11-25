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
    if !dir.try_exists()? {
        return Ok(vec![]);
    }
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
    let _lock = crate::library::lock_instance(root, item, "实例正在使用")?;
    let dir = item.game_dir(root)?.join("mods");
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
    let _lock = crate::library::lock_instance(root, item, "实例正在使用")?;
    let downloads = Downloads::new(cancel, &report)?;
    let mods = item.game_dir(root)?.join("mods");
    fs::create_dir_all(&mods)?;
    if let Some(project) = hit.project_id.strip_prefix("cf:") {
        return install_curse(&mods, item, project.parse()?, None, &downloads);
    }
    install_modrinth(&mods, item, &hit.project_id, None, &downloads)
}
pub fn install_pinned(
    mods: &Path,
    item: &Instance,
    file: &crate::market::Release,
    downloads: &Downloads,
) -> Result<()> {
    fs::create_dir_all(mods)?;
    match file.source {
        crate::market::Source::Modrinth => {
            install_modrinth(mods, item, &file.project, Some(file), downloads)
        }
        crate::market::Source::CurseForge => {
            install_curse(mods, item, file.project.parse()?, Some(file), downloads)
        }
    }
}

fn install_modrinth(
    mods: &Path,
    item: &Instance,
    project_id: &str,
    pinned: Option<&crate::market::Release>,
    downloads: &Downloads,
) -> Result<()> {
    let target = loader(item)?;
    let query = [
        (
            "game_versions",
            serde_json::json!([item.version]).to_string(),
        ),
        ("loaders", serde_json::json!([target]).to_string()),
    ];
    let mut queue = vec![(Some(project_id.to_owned()), pinned.map(|f| f.id.clone()))];
    let mut resolved = HashMap::new();
    let mut files = Vec::new();
    let mut names = HashSet::new();
    let temp = tempfile::tempdir_in(mods)?;
    (downloads.report)("解析模组及必需依赖".into());
    while let Some((project, exact)) = queue.pop() {
        downloads.check()?;
        if let Some(id) = project.as_ref().and_then(|id| resolved.get(id)) {
            ensure!(
                exact.as_ref().is_none_or(|wanted| wanted == id),
                "必需依赖的版本存在冲突"
            );
            continue;
        }
        let version: Version = if let Some(id) = &exact {
            ensure!(safe_id(id), "依赖版本 ID 无效");
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
            exact
                .as_ref()
                .is_none_or(|id| id == &version.id
                    && project.as_ref().is_none_or(|p| p == &version.project_id))
                && version.game_versions.contains(&item.version)
                && version.loaders.iter().any(|v| v == target),
            "依赖与当前实例不兼容"
        );
        if let Some(existing) = resolved.insert(version.project_id.clone(), version.id.clone()) {
            ensure!(existing == version.id, "项目要求的依赖版本冲突");
            continue;
        }
        let file = if let Some(pin) = pinned.filter(|f| f.id == version.id) {
            version
                .files
                .iter()
                .find(|f| f.filename == pin.filename && f.hashes.get("sha1") == Some(&pin.sha1))
        } else {
            version
                .files
                .iter()
                .filter(|f| jar_name(&f.filename))
                .max_by_key(|f| f.primary)
        }
        .context("发布文件已更改或缺少兼容的 JAR，请重新选择版本")?;
        ensure!(
            jar_name(&file.filename) && names.insert(file.filename.to_lowercase()),
            "模组文件名无效或重复"
        );
        let filename = download_name(&version.project_id, &file.filename)?;
        let hash = file.hashes.get("sha1").context("缺少文件校验值")?;
        let destination = mods.join(&filename);
        ensure!(
            !mods.join(format!("{}.disabled", filename)).exists(),
            "请先启用同名模组 {}",
            filename
        );
        if destination.exists() {
            ensure!(
                sha1_file(&destination)? == *hash,
                "同名模组文件内容不同：{}",
                filename
            );
        } else {
            files.push(DownloadTask {
                url: file.url.clone(),
                destination: temp.path().join(&filename),
                checksum: Some(Checksum::Sha1(hash.clone())),
                label: filename.clone(),
            });
        }
        if crate::preferences::current().dependencies {
            queue.extend(
                version
                    .dependencies
                    .into_iter()
                    .filter(|d| d.dependency_type == "required")
                    .map(|d| (d.project_id, d.version_id)),
            );
        }
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

fn install_curse(
    dir: &Path,
    item: &Instance,
    project: u64,
    pinned: Option<&crate::market::Release>,
    d: &Downloads,
) -> Result<()> {
    let mut seen = HashSet::new();
    let mut queue = vec![project];
    let mut files = vec![];
    let mut publish = vec![];
    let temp = tempfile::tempdir_in(dir)?;
    let mut names = HashSet::new();
    while let Some(id) = queue.pop() {
        d.check()?;
        if !seen.insert(id) {
            continue;
        }
        let mut f = match pinned.filter(|_| id == project) {
            Some(file) => {
                let f = crate::curse::pinned_jar(id, file.id.parse()?, item.loader, &item.version)?;
                ensure!(
                    f.name == file.filename && f.hash.eq_ignore_ascii_case(&file.sha1),
                    "发布文件已更改，请重新选择版本"
                );
                f
            }
            None => crate::curse::latest_jar(id, item.loader, &item.version)?,
        };
        f.name = download_name(&id.to_string(), &f.name)?;
        ensure!(
            jar_name(&f.name) && names.insert(f.name.to_lowercase()),
            "无效或重复的模组文件名"
        );
        let url = url::Url::parse(&f.url)?;
        ensure!(
            url.scheme() == "https" && url.host_str().is_some_and(|h| h.ends_with(".forgecdn.net")),
            "CurseForge 下载地址无效"
        );
        let to = dir.join(&f.name);
        ensure!(
            !dir.join(format!("{}.disabled", f.name)).exists(),
            "请先启用同名模组"
        );
        if to.try_exists()? {
            ensure!(sha1_file(&to)? == f.hash, "同名文件内容不同");
        } else {
            let tmp = temp.path().join(&f.name);
            publish.push((tmp.clone(), to));
            files.push(DownloadTask {
                url: f.url,
                destination: tmp,
                checksum: Some(Checksum::Sha1(f.hash)),
                label: f.name,
            });
        }
        if crate::preferences::current().dependencies {
            queue.extend(f.required);
        }
    }
    d.batch(temp.path(), "下载 CurseForge 模组", files)?;
    d.check()?;
    let mut created = vec![];
    for (from, to) in publish {
        if let Err(e) = fs::hard_link(from, &to) {
            for p in created {
                fs::remove_file(p)?;
            }
            return Err(e.into());
        }
        created.push(to);
    }
    Ok(())
}

#[derive(Clone)]
pub struct Upgrade {
    pub old: String,
    pub hash: String,
    pub new: String,
    pub hit: Hit,
}
pub fn upgrades(root: &Path, item: &Instance, curse: bool) -> Result<Vec<Upgrade>> {
    let dir = item.game_dir(root)?.join("mods");
    let mut local = HashMap::new();
    let mut prints = vec![];
    for name in installed(&dir)? {
        let path = dir.join(&name);
        let hash = sha1_file(&path)?;
        if curse {
            prints.push(crate::curse::fingerprint(&fs::read(path)?));
        }
        local.insert(hash, name);
    }
    if local.is_empty() {
        return Ok(vec![]);
    }
    let mut result = vec![];
    if curse {
        for (hash, (project, version)) in crate::curse::match_jars(&prints)? {
            if let Some(old) = local.get(&hash) {
                let f = crate::curse::latest_jar(project, item.loader, &item.version)?;
                if f.id > version && f.hash != hash {
                    result.push(Upgrade {
                        old: old.clone(),
                        hash,
                        new: download_name(&project.to_string(), &f.name)?,
                        hit: Hit {
                            project_id: format!("cf:{}", f.project),
                            title: f.name,
                        },
                    });
                }
            }
        }
    } else {
        let http = client()?;
        let keys: Vec<_> = local.keys().collect();
        for batch in keys.chunks(100) {
            let found:HashMap<String,Version>=http.post(format!("{API}/version_files/update")).json(&serde_json::json!({"hashes":batch,"algorithm":"sha1","loaders":[loader(item)?],"game_versions":[item.version]})).send()?.error_for_status()?.json()?;
            for (hash, v) in found {
                if let (Some(old), Some(f)) = (
                    local.get(&hash),
                    v.files.iter().find(|f| f.primary).or(v.files.first()),
                ) && v.files.iter().all(|f| f.hashes.get("sha1") != Some(&hash))
                {
                    result.push(Upgrade {
                        old: old.clone(),
                        hash,
                        new: download_name(&v.project_id, &f.filename)?,
                        hit: Hit {
                            project_id: v.project_id,
                            title: f.filename.clone(),
                        },
                    });
                }
            }
        }
    }
    result.sort_by(|a, b| a.old.cmp(&b.old));
    Ok(result)
}
pub fn upgrade(
    root: &Path,
    item: &Instance,
    u: &Upgrade,
    cancel: &AtomicBool,
    report: impl Fn(String) + Sync,
) -> Result<()> {
    let _lock = crate::library::lock_instance(root, item, "请先退出游戏")?;
    let dir = item.game_dir(root)?.join("mods");
    let d = Downloads::new(cancel, &report)?;
    replace_mod(&dir, &u.old, &u.hash, &u.new, &d, |staged| {
        if let Some(p) = u.hit.project_id.strip_prefix("cf:") {
            install_curse(staged, item, p.parse()?, None, &d)
        } else {
            install_modrinth(staged, item, &u.hit.project_id, None, &d)
        }
    })
}

pub fn replace_pinned(
    dir: &Path,
    item: &Instance,
    old: &crate::market::status::LocalFile,
    file: &crate::market::Release,
    d: &Downloads,
) -> Result<()> {
    let new = download_name(&file.project, &file.filename)?;
    replace_mod(dir, &old.name, &old.hash, &new, d, |staged| {
        install_pinned(staged, item, file, d)
    })
}

fn replace_mod(
    dir: &Path,
    old: &str,
    hash: &str,
    new: &str,
    d: &Downloads,
    install: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    ensure!(
        jar_name(new) && jar_name(old.strip_suffix(".disabled").unwrap_or(old)),
        "无效的模组文件名"
    );
    ensure!(
        sha1_file(dir.join(old))? == hash,
        "已安装文件发生变化，请重新检查更新"
    );
    let game = dir.parent().context("模组目录无上级")?;
    let temp = tempfile::tempdir_in(game)?;
    let staged = temp.path().join("mods");
    crate::backups::copy_tree(dir, &staged)?;
    fs::remove_file(staged.join(old))?;
    install(&staged)?;
    ensure!(staged.join(new).is_file(), "上游版本已更新，请重新检查");
    if old.ends_with(".disabled") {
        fs::rename(staged.join(new), staged.join(format!("{new}.disabled")))?;
    }
    d.check()?;
    let token = uuid::Uuid::new_v4().to_string();
    crate::library::atomic_write(&game.join(".noctiluca-mod-update"), token.as_bytes())?;
    let backup = game.join(format!("mods-before-update-{token}"));
    fs::rename(dir, &backup)?;
    if let Err(e) = fs::rename(staged, dir) {
        recover(game)?;
        return Err(e.into());
    }
    recover(game)?;
    Ok(())
}

fn download_name(project: &str, raw: &str) -> Result<String> {
    ensure!(
        jar_name(raw) && safe_id(project),
        "模组文件名或项目 ID 无效"
    );
    Ok(if crate::preferences::current().file_style == 1 {
        format!("[{project}] {raw}")
    } else {
        raw.into()
    })
}

pub fn describe(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(fs::File::open(path).ok()?).ok()?;
    for kind in ["fabric.mod.json", "quilt.mod.json"] {
        if let Ok(entry) = zip.by_name(kind)
            && entry.size() < 1024 * 1024
        {
            let doc: serde_json::Value = serde_json::from_reader(entry).ok()?;
            let data = doc.get("quilt_loader").unwrap_or(&doc);
            let name = data
                .pointer("/metadata/name")
                .or_else(|| data.get("name"))
                .or_else(|| data.get("id"))?
                .as_str()?;
            return Some(format!(
                "{name} · {}",
                data["version"].as_str().unwrap_or("?")
            ));
        }
    }
    for kind in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        if let Ok(entry) = zip.by_name(kind) {
            let mut meta = String::new();
            entry.take(512 * 1024).read_to_string(&mut meta).ok()?;
            let fields = meta.split("[[mods]]").nth(1)?.split("[[").next()?;
            let field = |key: &str| {
                fields
                    .lines()
                    .filter_map(|s| s.split_once('='))
                    .find(|(k, _)| k.trim() == key)
                    .map(|(_, v)| v.trim().trim_matches('"'))
            };
            let name = field("displayName").or_else(|| field("modId"))?;
            let v = field("version").unwrap_or("");
            return Some(if v.starts_with('$') || v.is_empty() {
                name.into()
            } else {
                format!("{name} · {v}")
            });
        }
    }
    None
}

pub fn recover(game: &Path) -> Result<()> {
    let marker = game.join(".noctiluca-mod-update");
    if !marker.try_exists()? {
        return Ok(());
    }
    let token = fs::read_to_string(&marker)?;
    let token = uuid::Uuid::parse_str(&token)?;
    let mods = game.join("mods");
    if !mods.try_exists()? {
        fs::rename(game.join(format!("mods-before-update-{token}")), mods)
            .context("恢复未完成的模组更新失败")?;
    }
    fs::remove_file(marker)?;
    Ok(())
}
