use crate::library::{self, Instance, Loader, Origin, safe_id};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) const CONTENT: &[&str] = &[
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
        Ok(data) => Ok(serde_json::from_slice::<Vec<PathBuf>>(&data)?
            .into_iter()
            .map(|p| dunce::simplified(&p).to_owned())
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e.into()),
    }
}
pub fn register(root: &Path, selected: &Path) -> Result<(PathBuf, Option<String>)> {
    let selected = dunce::canonicalize(selected).context("找不到选择的游戏目录")?;
    let mut profile = None;
    let folder = if selected.join("versions").is_dir() {
        selected
    } else if selected.join(".minecraft/versions").is_dir() {
        selected.join(".minecraft")
    } else if let (Some(versions), Some(name)) = (
        selected.parent(),
        selected.file_name().and_then(|s| s.to_str()),
    ) && versions
        .file_name()
        .is_some_and(|p| p.eq_ignore_ascii_case("versions"))
        && selected.join(format!("{name}.json")).is_file()
    {
        profile = Some(name.to_owned());
        versions.parent().context("缺少游戏根目录")?.to_owned()
    } else {
        anyhow::bail!("请选择 .minecraft、包含它的文件夹，或 versions 下的版本目录");
    };
    let _lock = crate::game::lock_file(&root.join("folders.lock"), "另一个窗口正在添加目录")?;
    let mut folders = registered(root)?;
    if !folders.iter().any(|p| dunce::simplified(p) == folder) {
        folders.push(folder.clone());
        library::atomic_write(
            &root.join("game-folders.json"),
            &serde_json::to_vec_pretty(&folders)?,
        )?;
    }
    Ok((folder, profile))
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
            let isolated = match crate::preferences::current().isolated {
                1 => true,
                2 => false,
                _ => isolated_layout(&entry.path()),
            };
            found.push(Found {
                origin: Origin {
                    folder: dunce::simplified(folder).to_owned(),
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

fn isolated_layout(dir: &Path) -> bool {
    if let Ok(config) = fs::read_to_string(dir.join("PCL/Setup.ini")) {
        for line in config.lines() {
            if let Some((key, value)) = line.split_once(':')
                && key.trim() == "VersionArgumentIndieV2"
            {
                if value.trim().eq_ignore_ascii_case("true") {
                    return true;
                }
                if value.trim().eq_ignore_ascii_case("false") {
                    return false;
                }
            }
        }
    }
    CONTENT.iter().any(|part| dir.join(part).exists())
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct External {
    pub origin: Origin,
    pub isolated: bool,
}

fn profile_id(name: &str) -> bool {
    Path::new(name).file_name().and_then(|s| s.to_str()) == Some(name)
        && !name.is_empty()
        && !name.contains(['/', '\\', ':'])
        && !name.ends_with([' ', '.'])
}

impl External {
    fn profile_path(&self, name: &str) -> Result<PathBuf> {
        ensure!(profile_id(name), "无效的版本目录名称");
        Ok(self
            .origin
            .folder
            .join("versions")
            .join(name)
            .join(format!("{name}.json")))
    }

    pub fn game_dir(&self) -> Result<PathBuf> {
        let file = self.profile_path(&self.origin.profile)?;
        ensure!(
            file.is_file(),
            "原目录中的版本文件已移动或不可用：{}",
            file.display()
        );
        Ok(if self.isolated {
            file.parent().unwrap().into()
        } else {
            self.origin.folder.clone()
        })
    }

    pub fn version(&self) -> Result<mc_launcher_core::core::version::VersionJson> {
        use mc_launcher_core::core::version::VersionJson;
        let mut chain = Vec::new();
        let mut names = std::collections::HashSet::new();
        let mut name = self.origin.profile.clone();
        loop {
            ensure!(
                names.insert(name.to_lowercase()) && chain.len() < 32,
                "本地版本配置存在循环或继承层级过多"
            );
            let path = self.profile_path(&name)?;
            let mut profile: VersionJson = serde_json::from_slice(
                &fs::read(&path)
                    .with_context(|| format!("找不到本地版本配置 {}", path.display()))?,
            )?;
            if let Some(jar) = &profile.jar {
                ensure!(profile_id(jar), "无效的客户端 JAR 路径");
            }
            profile.id = Some(name.clone());
            if let Some(parent) = profile.inherits_from.clone() {
                name = parent;
                chain.push(profile);
            } else {
                while let Some(child) = chain.pop() {
                    profile = profile.merge_child(&child);
                }
                return Ok(profile);
            }
        }
    }
}

pub fn link(root: &Path, found: &Found) -> Result<Instance> {
    let external = External {
        origin: found.origin.clone(),
        isolated: found.isolated,
    };
    external.game_dir()?;
    let identity = serde_json::to_vec(&external.origin)?;
    let mut item = Instance::new(
        found.origin.profile.clone(),
        found.version.clone(),
        found.loader,
    )?;
    item.id = format!("linked-{}", hex::encode(Sha256::digest(identity)));
    let _lock = crate::game::lock_file(&root.join("library.lock"), "其他窗口正在更新实例库")?;
    let file = item.path(root).join("instance.json");
    let removed = root.join("removed-instances").join(&item.id);
    if !file.try_exists()? && removed.join("instance.json").is_file() {
        fs::rename(&removed, item.path(root)).context("恢复原目录关联失败")?;
    }
    if file.try_exists()? {
        let expected_id = item.id.clone();
        item = serde_json::from_slice(&fs::read(file)?)?;
        ensure!(item.id == expected_id, "原目录关联无效");
        ensure!(
            item.external.as_ref().map(|e| &e.origin) == Some(&external.origin),
            "已有实例标识冲突"
        );
    }
    item.version = found.version.clone();
    item.loader = found.loader;
    item.loader_version = found.build.clone();
    item.external = Some(external);
    library::save(root, &item)?;
    Ok(item)
}
