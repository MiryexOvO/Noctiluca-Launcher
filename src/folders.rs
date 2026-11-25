use crate::library::{self, Instance, Loader, Origin, safe_id};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

const CONTENT: &[&str] = &[
    "saves",
    "mods",
    "config",
    "defaultconfigs",
    "resourcepacks",
    "shaderpacks",
    "screenshots",
    "schematics",
    "kubejs",
    "scripts",
    "patchouli_books",
    "global_packs",
    "resourcepackoverrides",
    "journeymap",
    "xaero",
    "XaeroWaypoints",
    "XaeroWorldMap",
    "options.txt",
    "optionsof.txt",
    "servers.dat",
];
#[derive(Clone)]
pub struct Found {
    pub origin: Origin,
    pub version: String,
    pub loader: Loader,
    pub build: Option<String>,
    pub isolated: bool,
}
pub fn registered(root: &Path) -> Result<Vec<PathBuf>> {
    match fs::read(root.join("game-folders.json")) {
        Ok(data) => Ok(serde_json::from_slice(&data)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e.into()),
    }
}
pub fn register(root: &Path, folder: &Path) -> Result<PathBuf> {
    let folder = folder.canonicalize()?;
    ensure!(
        folder.join("versions").is_dir(),
        "请选择含有 versions 子目录的游戏文件夹"
    );
    let _lock = crate::game::lock_file(&root.join("folders.lock"), "另一个窗口正在添加目录")?;
    let mut folders = registered(root)?;
    if !folders.contains(&folder) {
        folders.push(folder.clone());
        library::atomic_write(
            &root.join("game-folders.json"),
            &serde_json::to_vec_pretty(&folders)?,
        )?;
    }
    Ok(folder)
}
fn mc_version(id: &str) -> bool {
    safe_id(id)
        && id
            .split('.')
            .next()
            .and_then(|v| v.parse::<u32>().ok())
            .is_some_and(|n| n == 1 || n >= 26)
        && id.contains('.')
}
fn metadata(value: &Value, profile: &str) -> Option<(String, Loader, Option<String>)> {
    let id = value["id"].as_str().unwrap_or(profile);
    let mut base = value["inheritsFrom"]
        .as_str()
        .or_else(|| value["clientVersion"].as_str())
        .or_else(|| value["jar"].as_str())
        .filter(|s| mc_version(s))
        .map(str::to_owned);
    let mut loader = Loader::Vanilla;
    let mut build: Option<String> = None;
    if let Some(args) = value.pointer("/arguments/game").and_then(Value::as_array) {
        for pair in args.windows(2) {
            if let (Some(flag), Some(v)) = (pair[0].as_str(), pair[1].as_str())
                && safe_id(v)
            {
                match flag {
                    "--fml.neoForgeVersion" => {
                        loader = Loader::NeoForge;
                        build = Some(v.into());
                    }
                    "--fml.forgeVersion" => {
                        loader = Loader::Forge;
                        build = Some(v.into());
                    }
                    _ => {}
                }
            }
            if pair[0] == "--fml.mcVersion" && base.is_none() {
                base = pair[1]
                    .as_str()
                    .filter(|s| mc_version(s))
                    .map(str::to_owned);
            }
        }
    }
    if let Some(libs) = value["libraries"].as_array() {
        for name in libs.iter().filter_map(|l| l["name"].as_str()) {
            let mut parts = name.split(':');
            let (Some(group), Some(artifact), Some(v)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            match (group, artifact) {
                ("net.fabricmc", "intermediary") if base.is_none() && mc_version(v) => {
                    base = Some(v.into())
                }
                ("net.fabricmc", "fabric-loader") => {
                    loader = Loader::Fabric;
                    build = Some(v.into());
                }
                ("org.quiltmc", "quilt-loader") => {
                    loader = Loader::Quilt;
                    build = Some(v.into());
                }
                ("net.neoforged", "neoforge") => {
                    loader = Loader::NeoForge;
                    build = Some(v.into());
                }
                ("net.minecraftforge", "forge" | "fmlloader") => {
                    if let Some((mc, b)) = v.split_once('-') {
                        base.get_or_insert_with(|| mc.to_owned());
                        loader = Loader::Forge;
                        build = Some(b.into());
                    }
                }
                _ => {}
            }
        }
    }
    if base.is_none() {
        base = [id, profile]
            .into_iter()
            .find(|s| {
                mc_version(s)
                    && (s.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                        || value["type"] == "snapshot")
            })
            .map(str::to_owned);
    }
    let mc = base.filter(|v| safe_id(v))?;
    if loader == Loader::Vanilla
        && value["mainClass"].as_str().is_some_and(|main| {
            main != "net.minecraft.client.main.Main" && main != "net.minecraft.launchwrapper.Launch"
        })
    {
        return None;
    }
    Some((mc, loader, build.filter(|s| safe_id(s))))
}
pub fn scan(folder: &Path) -> Result<Vec<Found>> {
    let mut found = vec![];
    for entry in fs::read_dir(folder.join("versions"))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let profile = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path().join(format!("{profile}.json"));
        let Ok(stat) = fs::metadata(&path) else {
            continue;
        };
        if !stat.is_file() || stat.len() > 16 * 1024 * 1024 {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&fs::read(path)?) else {
            continue;
        };
        if let Some((version, loader, build)) = metadata(&value, &profile) {
            let isolated = CONTENT.iter().any(|p| entry.path().join(p).exists());
            found.push(Found {
                origin: Origin {
                    folder: folder.into(),
                    profile,
                },
                version,
                loader,
                build,
                isolated,
            });
        }
    }
    found.sort_by_key(|v| (v.loader.label(), v.origin.profile.to_lowercase()));
    Ok(found)
}
pub fn import(root: &Path, found: &Found) -> Result<Instance> {
    let folder = &found.origin.folder;
    ensure!(
        Path::new(&found.origin.profile)
            .file_name()
            .and_then(|s| s.to_str())
            == Some(found.origin.profile.as_str()),
        "无效的版本目录"
    );
    let mut item = Instance::new(
        found.origin.profile.clone(),
        found.version.clone(),
        found.loader,
    )?;
    item.loader_version = found.build.clone();
    item.origin = Some(found.origin.clone());
    let source = if found.isolated {
        folder.join("versions").join(&found.origin.profile)
    } else {
        folder.clone()
    };
    let temp = tempfile::tempdir_in(root)?;
    let game = temp.path().join("game");
    fs::create_dir(&game)?;
    for part in CONTENT {
        let from = source.join(part);
        if from.try_exists()? {
            ensure!(
                !temp
                    .path()
                    .canonicalize()?
                    .starts_with(from.canonicalize()?),
                "目标与来源目录重叠"
            );
            crate::backups::copy_tree(&from, &game.join(part))?;
        }
    }
    library::atomic_write(
        &temp.path().join("instance.json"),
        &serde_json::to_vec_pretty(&item)?,
    )?;
    fs::rename(temp.path(), item.path(root)).context("保存导入的独立实例失败")?;
    Ok(item)
}
