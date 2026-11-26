pub(crate) mod history;

use crate::{
    library::{self, Instance},
    market::icon::read_thumbnail,
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Local, Utc};
use flate2::read::GzDecoder;
use serde::Deserialize;
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
};
use walkdir::WalkDir;
use zip::{ZipWriter, write::SimpleFileOptions};

pub(crate) struct World {
    pub folder: OsString,
    pub name: String,
    pub played: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct Level {
    #[serde(rename = "Data")]
    data: WorldData,
}
#[derive(Deserialize, Default)]
struct WorldData {
    #[serde(rename = "LevelName", default)]
    name: String,
    #[serde(rename = "LastPlayed")]
    played: Option<i64>,
}

fn metadata(path: &Path) -> Option<WorldData> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || is_link(&meta) {
        return None;
    }
    let mut file = BufReader::new(File::open(path).ok()?);
    let gzip = file.fill_buf().ok()?.starts_with(&[0x1f, 0x8b]);
    let reader: Box<dyn Read> = if gzip {
        Box::new(GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    const LIMIT: u64 = 32 * 1024 * 1024;
    let mut bytes = Vec::new();
    reader.take(LIMIT + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > LIMIT {
        return None;
    }
    fastnbt::from_bytes::<Level>(&bytes).ok().map(|l| l.data)
}

fn is_link(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}

pub(crate) fn list(saves: &Path) -> Result<Vec<World>> {
    let mut worlds = Vec::new();
    for entry in fs::read_dir(saves)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if !meta.is_dir() || is_link(&meta) {
            continue;
        }
        let level = path.join("level.dat");
        let old = path.join("level.dat_old");
        if !level.is_file() && !old.is_file() {
            continue;
        }
        let data = metadata(&level)
            .or_else(|| metadata(&old))
            .unwrap_or_default();
        let folder = entry.file_name();
        let name = if data.name.trim().is_empty() {
            folder.to_string_lossy().into_owned()
        } else {
            data.name
        };
        let played = data
            .played
            .filter(|&t| t > 0)
            .and_then(DateTime::from_timestamp_millis);
        worlds.push(World {
            folder,
            name,
            played,
        });
    }
    worlds.sort_by(|a, b| {
        b.played
            .cmp(&a.played)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.folder.cmp(&b.folder))
    });
    Ok(worlds)
}

pub(crate) fn thumbnail(dir: &Path) -> Option<Vec<u8>> {
    let p = dir.join("icon.png");
    let meta = fs::symlink_metadata(&p).ok()?;
    if !meta.is_file() || is_link(&meta) {
        return None;
    }
    read_thumbnail(File::open(p).ok()?).ok()
}

pub(crate) fn backup_dir(root: &Path, item: &Instance) -> Result<PathBuf> {
    let path = item.game_dir(root)?.join("backups");
    fs::create_dir_all(&path)?;
    dunce::canonicalize(path).context("无法打开世界备份目录")
}

pub(crate) fn backup(root: &Path, item: &Instance, folder: &OsStr) -> Result<PathBuf> {
    let _instance = library::lock_instance(root, item, "请先退出游戏再备份存档")?;
    let name = Path::new(folder);
    ensure!(
        name.components().count() == 1
            && matches!(name.components().next(), Some(Component::Normal(_)))
            && name.file_name() == Some(folder),
        "无效的存档目录"
    );
    let saves = crate::content::resource_dir(root, item, "saves")?;
    let source = saves.join(folder);
    let meta = fs::symlink_metadata(&source)?;
    ensure!(
        meta.is_dir()
            && !is_link(&meta)
            && (source.join("level.dat").is_file() || source.join("level.dat_old").is_file()),
        "存档已移动或不可用"
    );
    let dest = backup_dir(root, item)?;
    ensure!(
        !dest.starts_with(dunce::canonicalize(&source)?),
        "备份目录位于存档内"
    );
    let session = source.join("session.lock");
    let lock = match File::open(&session) {
        Ok(file) => {
            file.try_lock().context("该世界正在使用，请退出后再备份")?;
            Some(file)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).context("无法检查世界是否正在使用"),
    };
    let mut tmp = tempfile::NamedTempFile::new_in(&dest)?;
    let mut zip = ZipWriter::new(tmp.as_file_mut());
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for entry in WalkDir::new(&source).follow_links(false) {
        let entry = entry?;
        let p = entry.path();
        if p == session {
            continue;
        }
        let meta = fs::symlink_metadata(p)?;
        ensure!(!is_link(&meta), "存档含有目录或文件链接：{}", p.display());
        let rel = p
            .strip_prefix(&saves)?
            .to_str()
            .context("存档文件名编码无效")?
            .replace('\\', "/");
        if meta.is_dir() {
            zip.add_directory(rel, options)?;
        } else {
            ensure!(meta.is_file(), "无法备份特殊文件：{}", p.display());
            zip.start_file(rel, options.large_file(meta.len() >= u32::MAX.into()))?;
            io::copy(&mut File::open(p)?, &mut zip)?;
        }
    }
    zip.finish()?.flush()?;
    tmp.as_file().sync_all()?;
    ensure!(
        lock.is_some() || !session.try_exists()?,
        "备份期间世界已被打开，请退出游戏并重试"
    );
    let folder_name = folder.to_string_lossy();
    let short_name = folder_name.chars().take(66).collect::<String>();
    let base = format!(
        "{}-{}",
        Local::now().format("%Y-%m-%d_%H-%M-%S"),
        short_name
    );
    for n in 0.. {
        let filename = if n == 0 {
            format!("{base}.zip")
        } else {
            format!("{base}-{n}.zip")
        };
        let path = dest.join(filename);
        match tmp.persist_noclobber(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => tmp = e.file,
            Err(e) => return Err(e.error.into()),
        }
    }
    unreachable!()
}
