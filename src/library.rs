use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Loader {
    Vanilla,
    Fabric,
    Quilt,
    Forge,
    NeoForge,
}

impl Loader {
    pub fn label(self) -> &'static str {
        match self {
            Self::Vanilla => "原版",
            Self::Fabric => "Fabric",
            Self::Quilt => "Quilt",
            Self::Forge => "Forge",
            Self::NeoForge => "NeoForge",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub version: String,
    pub loader: Loader,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loader_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<crate::folders::External>,
    pub memory_mib: u32,
    #[serde(default)]
    pub memory_mode: crate::memory::Mode,
    #[serde(default = "crate::memory::initial")]
    pub initial_mib: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jvm_args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<(u32, u32)>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Origin {
    pub folder: PathBuf,
    pub profile: String,
}

pub fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && !value.ends_with('.')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && !matches!(
            value
                .split('.')
                .next()
                .unwrap()
                .to_ascii_uppercase()
                .as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
}

impl Instance {
    pub fn new(name: String, version: String, loader: Loader) -> Result<Self> {
        ensure!(safe_id(&version), "版本号无效");
        ensure!(!name.trim().is_empty(), "请填写实例名称");
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            name: name.trim().to_owned(),
            version,
            loader,
            loader_version: None,
            origin: None,
            external: None,
            memory_mib: 2048,
            memory_mode: crate::memory::Mode::Global,
            initial_mib: 512,
            java_path: None,
            jvm_args: Vec::new(),
            resolution: None,
        })
    }

    pub fn game_dir(&self, root: &Path) -> Result<PathBuf> {
        match &self.external {
            Some(link) => link.game_dir(),
            None => Ok(self.path(root).join("game")),
        }
    }

    pub fn path(&self, root: &Path) -> PathBuf {
        root.join("instances").join(&self.id)
    }
}

// Keeping each instance in a separate file also keeps its identity independent of its display name.
pub fn save(root: &Path, instance: &Instance) -> Result<()> {
    ensure!(safe_id(&instance.id), "实例 ID 无效");
    let dir = instance.path(root);
    fs::create_dir_all(&dir)?;
    atomic_write(
        &dir.join("instance.json"),
        &serde_json::to_vec_pretty(instance)?,
    )
}

pub fn update(root: &Path, instance: &Instance) -> Result<()> {
    let _guard = crate::game::lock_file(&root.join("library.lock"), "其他窗口正在修改实例库")?;
    ensure!(
        safe_id(&instance.id) && instance.path(root).join("instance.json").is_file(),
        "实例已移除，请重新打开实例库"
    );
    save(root, instance)
}

pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().context("文件缺少上级目录")?;
    fs::create_dir_all(dir)?;
    let mut temp = NamedTempFile::new_in(dir)?;
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    Ok(())
}

fn portable_data(dir: &Path) -> Result<PathBuf> {
    let new = dir.join("Noctiluca-data");
    let legacy = dir.join("MCSTART-data");
    if !new.try_exists()? && legacy.is_dir() {
        Ok(legacy)
    } else {
        Ok(new)
    }
}

pub fn data_root() -> Result<PathBuf> {
    let path = match crate::setting("HOME") {
        Some(path) => std::path::absolute(path)?,
        None => portable_data(env::current_exe()?.parent().context("找不到程序目录")?)?,
    };
    fs::create_dir_all(&path).with_context(|| format!("无法创建数据目录 {}", path.display()))?;
    Ok(path)
}

pub fn cache_source(root: &Path) -> Result<Option<PathBuf>> {
    if let Some(path) = crate::setting("CACHE_SOURCE") {
        return Ok(Some(path.into()));
    }
    match fs::read(root.join("cache-source.json")) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub fn lock_instance(
    root: &Path,
    item: &Instance,
    reason: &str,
) -> Result<(fs::File, Option<fs::File>)> {
    let dir = item.path(root);
    let _library = crate::game::lock_file(&root.join("library.lock"), "其他窗口正在修改实例库")?;
    ensure!(
        dir.join("instance.json").is_file(),
        "实例已移除，请重新打开实例库"
    );
    let local = crate::game::lock_file(&dir.join("game.lock"), reason)?;
    let game = if let Some(link) = &item.external {
        Some(crate::game::lock_file(
            &link.game_dir()?.join(".noctiluca-game.lock"),
            "另一个操作正在使用同一游戏目录",
        )?)
    } else {
        None
    };
    Ok((local, game))
}

pub fn move_instance(root: &Path, id: &str, restore: bool) -> Result<()> {
    ensure!(safe_id(id), "实例 ID 无效");
    let _library = crate::game::lock_file(&root.join("library.lock"), "其他窗口正在修改实例库")?;
    let trash = root.join("removed-instances");
    fs::create_dir_all(&trash)?;
    let live = root.join("instances").join(id);
    let removed = trash.join(id);
    let (from, to) = if restore {
        (removed, live)
    } else {
        (live, removed)
    };
    ensure!(!to.try_exists()?, "目标中已有同名实例");
    let expected = from.parent().context("缺少实例目录")?.canonicalize()?;
    ensure!(
        from.canonicalize()?.parent() == Some(expected.as_path()),
        "实例目录不在实例库中"
    );
    let item: Instance = serde_json::from_slice(&fs::read(from.join("instance.json"))?)?;
    ensure!(item.id == id, "实例元数据不匹配");
    let game = crate::game::lock_file(
        &from.join("game.lock"),
        "此实例正在运行或安装，暂时无法移动",
    )?;
    // Windows requires all child handles closed for a directory rename.
    // The library lock prevents another operation from acquiring game.lock during the move.
    drop(game);
    fs::rename(from, to).context("移动实例失败")?;
    Ok(())
}
pub fn removed(root: &Path) -> Result<Vec<Instance>> {
    let dir = root.join("removed-instances");
    if !dir.try_exists()? {
        return Ok(vec![]);
    }
    let mut items = vec![];
    for entry in fs::read_dir(dir)? {
        let e = entry?;
        if e.file_type()?.is_dir() {
            let item: Instance =
                serde_json::from_slice(&fs::read(e.path().join("instance.json"))?)?;
            if safe_id(&item.id) && e.file_name().to_str() == Some(&item.id) {
                items.push(item);
            }
        }
    }
    items.sort_by_key(|i| i.name.clone());
    Ok(items)
}
pub fn load(root: &Path) -> Result<Vec<Instance>> {
    // Other launcher windows can create instances while this window is opening.
    let lock = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("library.lock"))?;
    lock.lock()?;
    let instances = root.join("instances");
    fs::create_dir_all(&instances)?;
    let mut result = Vec::new();
    for entry in fs::read_dir(&instances)? {
        let entry = entry?;
        let file = entry.path().join("instance.json");
        if !entry.file_type()?.is_dir() || !file.is_file() {
            continue;
        }
        let item: Instance = serde_json::from_slice(&fs::read(&file)?)
            .with_context(|| format!("实例配置损坏：{}", file.display()))?;
        ensure!(
            entry.file_name().to_str() == Some(&item.id)
                && safe_id(&item.id)
                && safe_id(&item.version),
            "实例路径或版本无效：{}",
            file.display()
        );
        ensure!(
            crate::memory::validate(item.initial_mib, item.memory_mib).is_ok(),
            "内存配置超出范围：{}",
            file.display()
        );
        result.push(item);
    }
    if !root.join("library-initialized").try_exists()? {
        for (id, loader) in [
            ("vanilla-1.20.1", Loader::Vanilla),
            ("fabric-1.20.1", Loader::Fabric),
        ] {
            if result.iter().any(|i| i.id == id) {
                continue;
            }
            let mut starter = Instance::new(
                format!("{} 1.20.1", loader.label()),
                "1.20.1".into(),
                loader,
            )?;
            starter.id = id.into();
            save(root, &starter)?;
            result.push(starter);
        }
        atomic_write(&root.join("library-initialized"), b"1")?;
    }
    result.sort_by_key(|i| (i.id != "vanilla-1.20.1", i.name.clone(), i.id.clone()));
    Ok(result)
}
