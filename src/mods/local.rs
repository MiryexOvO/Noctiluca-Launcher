use anyhow::Result;
use serde::Deserialize;
use std::{fs::File, io::Read, path::Path};
use zip::ZipArchive;

#[derive(Default)]
pub(crate) struct LocalMod {
    pub file: String,
    pub entries: Vec<Entry>,
}

#[derive(Deserialize)]
pub(crate) struct Entry {
    #[serde(rename = "modId", alias = "id")]
    pub id: String,
    #[serde(default, rename = "displayName", alias = "name")]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Filter {
    #[default]
    All,
    Enabled,
    Disabled,
    Updates,
}

impl LocalMod {
    pub fn disabled(&self) -> bool {
        self.file.ends_with(".disabled")
    }

    pub fn matches(&self, query: &str, filter: Filter, update: bool) -> bool {
        let visible = match filter {
            Filter::All => true,
            Filter::Enabled => !self.disabled(),
            Filter::Disabled => self.disabled(),
            Filter::Updates => update,
        };
        visible
            && (query.is_empty()
                || self.file.to_lowercase().contains(query)
                || self.entries.iter().any(|e| {
                    e.name.to_lowercase().contains(query) || e.id.to_lowercase().contains(query)
                }))
    }

    pub fn label(&self) -> String {
        self.entries
            .iter()
            .map(|e| {
                let name = if e.name.is_empty() { &e.id } else { &e.name };
                if e.version.is_empty() {
                    name.clone()
                } else {
                    format!("{name} {}", e.version)
                }
            })
            .collect::<Vec<_>>()
            .join("、")
    }
}

fn entry_text(zip: &mut ZipArchive<File>, name: &str) -> Option<String> {
    let entry = zip.by_name(name).ok()?;
    const LIMIT: u64 = 1024 * 1024;
    if entry.size() > LIMIT {
        return None;
    }
    let mut text = String::new();
    entry.take(LIMIT + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= LIMIT).then_some(text)
}

fn metadata(path: &Path) -> Option<Vec<Entry>> {
    let mut zip = ZipArchive::new(File::open(path).ok()?).ok()?;
    if let Some(text) = entry_text(&mut zip, "fabric.mod.json")
        && let Ok(entry) = serde_json::from_str(&text)
    {
        return Some(vec![entry]);
    }
    if let Some(text) = entry_text(&mut zip, "quilt.mod.json")
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
        && let Some(loader) = json.get("quilt_loader")
        && let Ok(mut entry) = serde_json::from_value::<Entry>(loader.clone())
    {
        if let Some(name) = loader.pointer("/metadata/name").and_then(|s| s.as_str()) {
            entry.name = name.into();
        }
        return Some(vec![entry]);
    }
    #[derive(Deserialize)]
    struct Forge {
        mods: Vec<Entry>,
    }
    for descriptor in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        if let Some(text) = entry_text(&mut zip, descriptor)
            && let Ok(mut forge) = toml::from_str::<Forge>(&text)
        {
            if forge.mods.iter().any(|e| e.version.starts_with('$')) {
                let manifest = entry_text(&mut zip, "META-INF/MANIFEST.MF").unwrap_or_default();
                let version = manifest
                    .lines()
                    .take_while(|l| !l.is_empty())
                    .find_map(|line| line.strip_prefix("Implementation-Version:").map(str::trim));
                for e in &mut forge.mods {
                    if e.version == "${file.jarVersion}" {
                        e.version = version.unwrap_or_default().into();
                    } else if e.version.starts_with('$') {
                        e.version.clear();
                    }
                }
            }
            return Some(forge.mods);
        }
    }
    None
}

pub(crate) fn inventory(dir: &Path) -> Result<Vec<LocalMod>> {
    Ok(super::installed(dir)?
        .into_iter()
        .map(|file| {
            let entries = metadata(&dir.join(&file)).unwrap_or_default();
            LocalMod { file, entries }
        })
        .collect())
}
