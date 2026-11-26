use anyhow::Result;
use serde::Deserialize;
use std::{fs::File, io::Read, path::Path};
use zip::ZipArchive;

#[derive(Default)]
pub(crate) struct LocalMod {
    pub file: String,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Deserialize)]
pub(crate) struct Entry {
    #[serde(rename = "modId", alias = "id", alias = "modid")]
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

pub(super) fn entry_text<R: Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    name: &str,
) -> Option<String> {
    let entry = zip.by_name(name).ok()?;
    const LIMIT: u64 = 1024 * 1024;
    if entry.size() > LIMIT {
        return None;
    }
    let mut text = String::new();
    entry.take(LIMIT + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= LIMIT).then_some(text)
}

#[derive(Default)]
pub(crate) struct Descriptor {
    pub entries: Vec<Entry>,
    platforms: Vec<Platform>,
}
// Each required Minecraft dependency can list alternative version ranges.
struct Platform {
    loaders: Vec<crate::library::Loader>,
    requirements: Vec<Vec<String>>,
}
impl Descriptor {
    fn platform(
        &mut self,
        loaders: Vec<crate::library::Loader>,
        entries: Vec<Entry>,
        requirements: Vec<Vec<String>>,
    ) {
        self.platforms.push(Platform {
            loaders,
            requirements,
        });
        for e in entries {
            if !self.entries.iter().any(|v| v.id == e.id) {
                self.entries.push(e);
            }
        }
    }
    pub fn compatibility(&self, item: &crate::library::Instance) -> Result<Option<String>, String> {
        use crate::library::Loader;
        if item.loader == Loader::Vanilla {
            return Err("原版实例没有模组加载器".into());
        }
        if self.platforms.is_empty() {
            return Ok(Some("未识别到模组声明，请自行确认适用版本".into()));
        }
        let mut supported = vec![];
        let mut failed = vec![];
        let mut advisory = None;
        for platform in &self.platforms {
            for loader in &platform.loaders {
                if !supported.contains(&loader.label()) {
                    supported.push(loader.label());
                }
            }
            if !platform.loaders.contains(&item.loader) {
                continue;
            }
            let mut uncertain = None;
            let mut rejected = false;
            for alternatives in &platform.requirements {
                if alternatives.is_empty() {
                    uncertain = Some("模组未声明游戏版本".to_string());
                    continue;
                }
                let matches = alternatives
                    .iter()
                    .map(|s| super::versions::matches(s, &item.version))
                    .collect::<Vec<_>>();
                if matches.contains(&Some(true)) {
                    continue;
                }
                let range = alternatives.join(" 或 ");
                if matches.contains(&None) {
                    uncertain = Some(format!("请核对游戏版本要求 {range}"));
                } else {
                    failed.push(range);
                    rejected = true;
                }
            }
            if !rejected {
                if platform.requirements.is_empty() {
                    uncertain = Some("模组未声明游戏版本".into());
                }
                if uncertain.is_none() {
                    return Ok(None);
                }
                advisory = uncertain;
            }
        }
        if advisory.is_some() {
            Ok(advisory)
        } else if failed.is_empty() {
            Err(format!(
                "此模组声明支持 {}，当前实例使用 {}",
                supported.join(" / "),
                item.loader.label()
            ))
        } else {
            Err(format!(
                "模组要求 Minecraft {}，当前实例是 {}",
                failed.join("、"),
                item.version
            ))
        }
    }
}
fn strings(v: Option<&serde_json::Value>) -> Vec<String> {
    match v {
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => vec![],
    }
}
pub(crate) fn describe(path: &Path) -> Descriptor {
    use crate::library::Loader::*;
    let mut out = Descriptor::default();
    let Ok(file) = File::open(path) else {
        return out;
    };
    let Ok(mut zip) = ZipArchive::new(file) else {
        return out;
    };
    if let Some(json) = entry_text(&mut zip, "fabric.mod.json")
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        && let Ok(e) = serde_json::from_value::<Entry>(json.clone())
    {
        out.platform(
            vec![Fabric, Quilt],
            vec![e],
            vec![strings(json.pointer("/depends/minecraft"))],
        );
    }
    if let Some(json) = entry_text(&mut zip, "quilt.mod.json")
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        && let Some(loader) = json.get("quilt_loader")
        && let Ok(mut e) = serde_json::from_value::<Entry>(loader.clone())
    {
        if let Some(n) = loader.pointer("/metadata/name").and_then(|v| v.as_str()) {
            e.name = n.into();
        }
        let versions = loader
            .get("depends")
            .and_then(|v| v.as_array())
            .and_then(|a| a.iter().find(|v| v["id"] == "minecraft"))
            .map(|v| strings(v.get("versions")))
            .unwrap_or_default();
        out.platform(vec![Quilt], vec![e], vec![versions]);
    }
    #[derive(Deserialize, Default)]
    struct ForgeMeta {
        mods: Vec<Entry>,
        #[serde(default)]
        dependencies: std::collections::HashMap<String, Vec<Dependency>>,
    }
    #[derive(Deserialize)]
    struct Dependency {
        #[serde(rename = "modId")]
        id: String,
        #[serde(rename = "versionRange", default)]
        range: String,
        #[serde(rename = "type", default)]
        kind: String,
        mandatory: Option<bool>,
    }
    for name in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        if let Some(mut meta) =
            entry_text(&mut zip, name).and_then(|s| toml::from_str::<ForgeMeta>(&s).ok())
        {
            let deps: Vec<_> = meta
                .dependencies
                .values()
                .flatten()
                .filter(|d| {
                    d.mandatory != Some(false) && (d.kind.is_empty() || d.kind == "required")
                })
                .collect();
            let loaders = if name.contains("neoforge") || deps.iter().any(|d| d.id == "neoforge") {
                vec![NeoForge]
            } else if deps.iter().any(|d| d.id == "forge") {
                vec![Forge]
            } else {
                vec![Forge, NeoForge]
            };
            if meta.mods.iter().any(|e| e.version.starts_with('$')) {
                let manifest = entry_text(&mut zip, "META-INF/MANIFEST.MF").unwrap_or_default();
                let version = manifest
                    .lines()
                    .take_while(|l| !l.is_empty())
                    .find_map(|l| l.strip_prefix("Implementation-Version:").map(str::trim))
                    .unwrap_or_default();
                for e in &mut meta.mods {
                    if e.version.starts_with('$') {
                        e.version = if e.version == "${file.jarVersion}" {
                            version.into()
                        } else {
                            String::new()
                        };
                    }
                }
            }
            out.platform(
                loaders,
                meta.mods,
                deps.iter()
                    .filter(|d| d.id == "minecraft" && !d.range.is_empty())
                    .map(|d| vec![d.range.clone()])
                    .collect(),
            );
        }
    }
    if let Some(json) = entry_text(&mut zip, "mcmod.info")
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        && let Some(rows) = json.as_array().or_else(|| json["modList"].as_array())
    {
        let mut entries = Vec::new();
        let mut requirements = Vec::new();
        for row in rows {
            if let Ok(e) = serde_json::from_value::<Entry>(row.clone()) {
                entries.push(e);
                requirements.push(strings(row.get("mcversion")));
            }
        }
        if !entries.is_empty() {
            out.platform(vec![Forge], entries, requirements);
        }
    }
    out
}

pub(crate) fn inventory(dir: &Path) -> Result<Vec<LocalMod>> {
    Ok(super::installed(dir)?
        .into_iter()
        .map(|file| {
            let entries = describe(&dir.join(&file)).entries;
            LocalMod { file, entries }
        })
        .collect())
}
