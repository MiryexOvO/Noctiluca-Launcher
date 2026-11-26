use crate::{
    content::Upstream,
    library::Instance,
    market::{
        Source,
        icon::{cached, read_thumbnail},
    },
};
use serde::Deserialize;
use std::{
    fs::{self, File},
    io::Read,
    path::Path,
    time::Duration,
};
use zip::ZipArchive;

pub(crate) const FILENAME: &str = "pack-icon.png";

pub(crate) fn from_archive(zip: &mut ZipArchive<File>) -> Option<Vec<u8>> {
    let mut prefix = String::new();
    let mut names = Vec::new();
    let prism = if zip.index_for_name("modrinth.index.json").is_some() {
        None
    } else {
        zip.file_names()
            .find(|n| *n == "mmc-pack.json" || n.ends_with("/mmc-pack.json"))
            .and_then(|n| n.strip_suffix("mmc-pack.json"))
            .map(str::to_owned)
    };
    if let Some(dir) = prism {
        prefix = dir;
        let mut cfg = String::new();
        if let Ok(entry) = zip.by_name(&format!("{prefix}instance.cfg"))
            && entry.size() <= 256 * 1024
            && entry.take(256 * 1024).read_to_string(&mut cfg).is_ok()
            && let Some(key) = cfg
                .lines()
                .find_map(|line| line.strip_prefix("iconKey="))
                .map(str::trim)
            && !key.is_empty()
            && !key.contains('/')
            && crate::packs::relative(key).is_ok()
        {
            names.push(format!("{prefix}{key}.png"));
            names.push(format!("{prefix}icons/{key}.png"));
        }
    }
    for name in [FILENAME, "icon.png", "pack.png"] {
        names.push(format!("{prefix}{name}"));
    }
    names.into_iter().find_map(|name| {
        let entry = zip.by_name(&name).ok()?;
        if entry.is_symlink() || !entry.is_file() {
            return None;
        }
        read_thumbnail(entry).ok()
    })
}

fn local_file(base: &Path, relative: &str) -> Option<File> {
    let relative = crate::packs::relative(relative).ok()?;
    let path = dunce::canonicalize(base.join(relative)).ok()?;
    if !path.starts_with(dunce::canonicalize(base).ok()?) || !path.is_file() {
        return None;
    }
    File::open(path).ok()
}

fn game_icon(game: &Path) -> Option<Vec<u8>> {
    let read = |rel: &str| read_thumbnail(local_file(game, rel)?).ok();
    if let Some(icon) = ["PCL/Logo.png", "icon.png", FILENAME, "pack.png"]
        .into_iter()
        .find_map(read)
    {
        return Some(icon);
    }
    let mut config = String::new();
    let options = local_file(game, "config/fancymenu/options.txt")?;
    if options.metadata().ok()?.len() > 256 * 1024 {
        return None;
    }
    options.take(256 * 1024).read_to_string(&mut config).ok()?;
    let value = |key: &str| {
        config.lines().find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == key)
                .then(|| value.trim().trim_end_matches(';').trim().trim_matches('\''))
        })
    };
    if value("B:show_custom_window_icon") != Some("true") {
        return None;
    }
    ["S:custom_window_icon_32", "S:custom_window_icon_16"]
        .into_iter()
        .filter_map(value)
        .find_map(|p| read(p.strip_prefix('/').unwrap_or(p)))
}

pub(crate) fn installed_icon(
    game: &Path,
    external: Option<&crate::folders::External>,
) -> Option<Vec<u8>> {
    if let Some(link) = external {
        let profile = link
            .origin
            .folder
            .join("versions")
            .join(&link.origin.profile);
        if profile != game
            && let Some(icon) = game_icon(&profile)
        {
            return Some(icon);
        }
    }
    game_icon(game)
}

fn project_icon(root: &Path, upstream: &Upstream) -> Option<Vec<u8>> {
    if !crate::library::safe_id(&upstream.project) {
        return None;
    }
    let saved = root.join("cache/project-icons").join(format!(
        "{}-{}.url",
        upstream.source.label(),
        upstream.project
    ));
    let saved_url = fs::read_to_string(&saved).ok();
    let url = upstream
        .icon_url
        .clone()
        .or_else(|| saved_url.clone())
        .or_else(|| match upstream.source {
            Source::Modrinth => {
                #[derive(Deserialize)]
                struct Project {
                    icon_url: Option<String>,
                }
                crate::download::client()
                    .ok()?
                    .get(format!(
                        "https://api.modrinth.com/v2/project/{}",
                        upstream.project
                    ))
                    .timeout(Duration::from_secs(8))
                    .send()
                    .ok()?
                    .error_for_status()
                    .ok()?
                    .json::<Project>()
                    .ok()?
                    .icon_url
            }
            Source::CurseForge => crate::curse::project_icon(&upstream.project).ok().flatten(),
        })?;
    let path = cached(root, &url).ok()?;
    if saved_url.as_deref() != Some(&url) {
        let _ = crate::library::atomic_write(&saved, url.as_bytes());
    }
    read_thumbnail(File::open(path).ok()?).ok()
}

pub(crate) fn load(root: &Path, id: &str) -> Option<Vec<u8>> {
    if !crate::library::safe_id(id) {
        return None;
    }
    let dir = root.join("instances").join(id);
    if let Some(icon) = local_file(&dir, FILENAME).and_then(|f| read_thumbnail(f).ok()) {
        return Some(icon);
    }
    let item: Instance =
        serde_json::from_reader(File::open(dir.join("instance.json")).ok()?).ok()?;
    if item.id != id {
        return None;
    }
    let game = item.game_dir(root).ok()?;
    if let Some(icon) = installed_icon(&game, item.external.as_ref()) {
        return Some(icon);
    }
    #[derive(Deserialize)]
    struct Pack {
        upstream: Option<Upstream>,
        archive_sha1: Option<String>,
    }
    let pack: Pack = serde_json::from_reader(File::open(dir.join("pack-state.json")).ok()?).ok()?;
    if let Some(upstream) = pack.upstream {
        return project_icon(root, &upstream);
    }
    let hash = pack.archive_sha1?;
    if hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let image = root
        .join("cache/project-icons")
        .join(format!("archive-{}.png", hash.to_ascii_lowercase()));
    if let Some(icon) = File::open(&image).ok().and_then(|f| read_thumbnail(f).ok()) {
        return Some(icon);
    }
    let upstream = crate::market::lookup_pack_hash(&hash).ok()?;
    let icon = project_icon(root, &upstream)?;
    let _ = crate::library::atomic_write(&image, &icon);
    Some(icon)
}
