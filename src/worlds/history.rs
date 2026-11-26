use super::*;
use anyhow::{anyhow, bail};
use std::collections::HashSet;
use zip::ZipArchive;

#[derive(Clone)]
pub(crate) struct Record {
    pub path: PathBuf,
    pub folder: String,
    pub modified: std::time::SystemTime,
}

fn archive_world(zip: &ZipArchive<File>) -> Result<String> {
    let mut roots = zip.file_names().filter_map(|name| {
        let (first, rest) = name.split_once('/')?;
        matches!(rest, "level.dat" | "level.dat_old").then_some(first)
    });
    let root = roots.next().context("备份中没有世界数据")?;
    ensure!(roots.all(|s| s == root), "压缩包里有多个世界");
    crate::packs::relative(root)?;
    Ok(root.to_owned())
}

pub(crate) fn records(root: &Path, item: &Instance, folder: Option<&OsStr>) -> Result<Vec<Record>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(backup_dir(root, item)?)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if !meta.is_file()
            || is_link(&meta)
            || !path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("zip"))
        {
            continue;
        }
        let Ok(world) = File::open(&path)
            .map_err(anyhow::Error::from)
            .and_then(|f| archive_world(&ZipArchive::new(f)?))
        else {
            continue;
        };
        if folder.is_none_or(|v| v == OsStr::new(&world)) {
            out.push(Record {
                path,
                folder: world,
                modified: meta.modified()?,
            });
        }
    }
    out.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| b.path.cmp(&a.path))
    });
    Ok(out)
}

fn rename_level(dir: &Path, suffix: &str, fallback: &str) -> Result<String> {
    fn read_level(path: &Path) -> Result<fastnbt::Value> {
        let mut reader = BufReader::new(File::open(path)?);
        let gzip = reader.fill_buf()?.starts_with(&[0x1f, 0x8b]);
        let mut input: Box<dyn Read> = if gzip {
            Box::new(GzDecoder::new(reader))
        } else {
            Box::new(reader)
        };
        let mut buf = vec![];
        input
            .by_ref()
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut buf)?;
        ensure!(buf.len() <= 32 * 1024 * 1024, "世界信息过大");
        Ok(fastnbt::from_bytes(&buf)?)
    }
    for name in ["level.dat", "level.dat_old"] {
        let Ok(fastnbt::Value::Compound(mut root)) = read_level(&dir.join(name)) else {
            continue;
        };
        let Some(fastnbt::Value::Compound(data)) = root.get_mut("Data") else {
            continue;
        };
        let old = match data.get("LevelName") {
            Some(fastnbt::Value::String(s)) if !s.trim().is_empty() => s,
            _ => fallback,
        };
        let title = format!("{old}（{suffix}）");
        data.insert("LevelName".into(), fastnbt::Value::String(title.clone()));
        let mut encoder = flate2::write::GzEncoder::new(
            File::create(dir.join("level.dat"))?,
            flate2::Compression::fast(),
        );
        encoder.write_all(&fastnbt::to_bytes(&fastnbt::Value::Compound(root))?)?;
        encoder.finish()?.sync_all()?;
        return Ok(title);
    }
    bail!("备份中的世界信息损坏，无法恢复")
}

pub(crate) fn restore(root: &Path, item: &Instance, record: &Record) -> Result<String> {
    let _lock = library::lock_instance(root, item, "请先退出游戏再恢复存档")?;
    let backups = backup_dir(root, item)?;
    ensure!(
        record.path.parent() == Some(backups.as_path()),
        "备份不属于此实例"
    );
    let meta = fs::symlink_metadata(&record.path)?;
    ensure!(
        meta.is_file() && !is_link(&meta) && meta.modified()? == record.modified,
        "备份文件已变更，请重新打开备份记录"
    );
    let mut archive = ZipArchive::new(File::open(&record.path)?)?;
    ensure!(
        archive_world(&archive)? == record.folder,
        "备份对应的世界已变更"
    );
    let saves = crate::content::resource_dir(root, item, "saves")?;
    let tmp = tempfile::Builder::new()
        .prefix(".noctiluca-restore-")
        .tempdir_in(&saves)?;
    let stage = tmp.path().join("world");
    fs::create_dir(&stage)?;
    let mut seen = HashSet::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().trim_end_matches('/');
        let path = crate::packs::relative(name)?;
        let sub = path
            .strip_prefix(&record.folder)
            .map_err(|_| anyhow!("备份包含世界目录以外的文件"))?;
        if sub.as_os_str().is_empty() {
            ensure!(entry.is_dir(), "世界根目录无效");
            continue;
        }
        let mode = entry.unix_mode().unwrap_or(0) & 0o170000;
        ensure!(
            matches!(mode, 0 | 0o100000 | 0o040000),
            "备份包含链接或特殊文件"
        );
        ensure!(
            seen.insert(sub.to_string_lossy().to_lowercase()),
            "备份有重名文件"
        );
        if sub.as_os_str().eq_ignore_ascii_case("session.lock") {
            continue;
        }
        let dest = stage.join(sub);
        if entry.is_dir() {
            fs::create_dir_all(dest)?;
        } else {
            fs::create_dir_all(dest.parent().context("无效的存档路径")?)?;
            let size = entry.size();
            let mut file = File::create_new(dest)?;
            ensure!(io::copy(&mut entry, &mut file)? == size, "备份文件不完整");
        }
    }
    let day = Local::now().format("%Y-%m-%d").to_string();
    let short = record.folder.chars().take(66).collect::<String>();
    for n in 1.. {
        let suffix = if n == 1 {
            format!("恢复 {day}")
        } else {
            format!("恢复 {day} ({n})")
        };
        let dest = saves.join(format!("{short} - {suffix}"));
        if dest.try_exists()? {
            continue;
        }
        let title = rename_level(&stage, &suffix, &record.folder)?;
        fs::rename(&stage, &dest).context("无法保存恢复的世界，请刷新记录后重试")?;
        return Ok(title);
    }
    unreachable!()
}
