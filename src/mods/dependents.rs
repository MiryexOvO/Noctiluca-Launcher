use super::{installed, local::entry_text};
use crate::library::{Instance, Loader};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Cursor, Read, Seek},
    path::Path,
};
use zip::ZipArchive;

#[derive(Default)]
struct Manifest {
    ids: BTreeSet<String>,
    needs: Vec<Vec<String>>,
    name: String,
}
fn metadata<R: Read + Seek>(zip: &mut ZipArchive<R>, file: &str) -> Option<Value> {
    serde_json::from_str(&entry_text(zip, file)?).ok()
}
fn quilt_ids(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().flat_map(quilt_ids).collect(),
        Value::Object(m)
            if m.get("optional") != Some(&Value::Bool(true)) && !m.contains_key("unless") =>
        {
            m.get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .into_iter()
                .collect()
        }
        _ => vec![],
    }
}
fn fabric<R: Read + Seek>(
    z: &mut ZipArchive<R>,
    loader: Loader,
    m: &mut Manifest,
    budget: &mut u64,
    depth: usize,
) {
    let mut nested = vec![];
    if loader == Loader::Quilt
        && let Some(q) = metadata(z, "quilt.mod.json").and_then(|v| v.get("quilt_loader").cloned())
    {
        if let Some(id) = q["id"].as_str() {
            m.ids.insert(id.into());
        }
        if m.name.is_empty() {
            m.name = q
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into();
        }
        for p in q["provides"].as_array().into_iter().flatten() {
            m.ids.extend(quilt_ids(p));
        }
        for d in q["depends"].as_array().into_iter().flatten() {
            let choices = quilt_ids(d);
            if !choices.is_empty() {
                m.needs.push(choices);
            }
        }
        nested.extend(
            q["jars"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
    } else if let Some(f) = metadata(z, "fabric.mod.json") {
        if f["environment"] == "server" {
            return;
        }
        if let Some(id) = f["id"].as_str() {
            m.ids.insert(id.into());
        }
        if m.name.is_empty() {
            m.name = f["name"].as_str().unwrap_or_default().into();
        }
        for id in f["provides"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            m.ids.insert(id.into());
        }
        if let Some(deps) = f["depends"].as_object() {
            m.needs.extend(deps.keys().map(|id| vec![id.clone()]));
        }
        nested.extend(
            f["jars"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|j| j["file"].as_str())
                .map(str::to_owned),
        );
    }
    read_nested(z, nested, budget, depth, |inner, b, d| {
        fabric(inner, loader, m, b, d)
    });
}
fn read_nested<R: Read + Seek>(
    z: &mut ZipArchive<R>,
    names: Vec<String>,
    budget: &mut u64,
    depth: usize,
    mut parse: impl FnMut(&mut ZipArchive<Cursor<Vec<u8>>>, &mut u64, usize),
) {
    if depth >= 3 {
        return;
    }
    for name in names {
        let Ok(entry) = z.by_name(&name) else {
            continue;
        };
        let size = entry.size();
        if size > *budget || size > 32 * 1024 * 1024 {
            continue;
        }
        *budget -= size;
        let mut data = Vec::new();
        if entry.take(size + 1).read_to_end(&mut data).is_ok()
            && data.len() as u64 == size
            && let Ok(mut inner) = ZipArchive::new(Cursor::new(data))
        {
            parse(&mut inner, budget, depth + 1);
        }
    }
}
fn manifest(path: &Path, loader: Loader) -> Manifest {
    let mut m = Manifest::default();
    let Some(mut z) = File::open(path).ok().and_then(|f| ZipArchive::new(f).ok()) else {
        return m;
    };
    if matches!(loader, Loader::Fabric | Loader::Quilt) {
        fabric(&mut z, loader, &mut m, &mut (96 * 1024 * 1024), 0);
        return m;
    }
    forge(&mut z, loader, &mut m, &mut (96 * 1024 * 1024), 0);
    m
}
fn forge<R: Read + Seek>(
    z: &mut ZipArchive<R>,
    loader: Loader,
    m: &mut Manifest,
    budget: &mut u64,
    depth: usize,
) {
    let toml = if loader == Loader::NeoForge {
        entry_text(z, "META-INF/neoforge.mods.toml")
    } else {
        None
    }
    .or_else(|| entry_text(z, "META-INF/mods.toml"));
    if let Some(t) = toml.and_then(|s| s.parse::<toml::Table>().ok()) {
        for e in t
            .get("mods")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(id) = e.get("modId").and_then(|v| v.as_str()) {
                m.ids.insert(id.into());
            }
            if m.name.is_empty() {
                m.name = e
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .into();
            }
        }
        for group in t
            .get("dependencies")
            .and_then(|v| v.as_table())
            .into_iter()
            .flat_map(|v| v.values())
        {
            for d in group.as_array().into_iter().flatten() {
                if d.get("mandatory").and_then(|v| v.as_bool()) == Some(false)
                    || d.get("type")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| s != "required")
                    || d.get("side").and_then(|v| v.as_str()) == Some("SERVER")
                {
                    continue;
                }
                if let Some(id) = d.get("modId").and_then(|v| v.as_str()) {
                    m.needs.push(vec![id.into()]);
                }
            }
        }
    } else if let Some(meta) = metadata(z, "mcmod.info") {
        for row in meta
            .as_array()
            .or_else(|| meta["modList"].as_array())
            .into_iter()
            .flatten()
        {
            if let Some(id) = row["modid"].as_str() {
                m.ids.insert(id.into());
            }
            if m.name.is_empty() {
                m.name = row["name"].as_str().unwrap_or_default().into();
            }
            for required in row["requiredMods"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                let id = required.split('@').next().unwrap_or(required);
                m.needs.push(vec![
                    id.trim_start_matches("required-after:")
                        .trim_start_matches("required-before:")
                        .into(),
                ]);
            }
        }
    }
    if let Some(metadata) = metadata(z, "META-INF/jarjar/metadata.json") {
        let nested = metadata["jars"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|j| j["path"].as_str().map(str::to_owned))
            .collect();
        read_nested(z, nested, budget, depth, |inner, b, d| {
            forge(inner, loader, m, b, d)
        });
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Toggle {
    pub name: String,
    pub affected: BTreeMap<String, String>,
}
pub fn inspect(dir: &Path, loader: Loader, name: &str) -> Result<Toggle> {
    ensure!(
        super::jar_name(name.strip_suffix(".disabled").unwrap_or(name)) && dir.join(name).is_file(),
        "找不到所选模组"
    );
    let mut affected = BTreeMap::new();
    if !name.ends_with(".disabled") {
        let entries: BTreeMap<_, _> = installed(dir)?
            .into_iter()
            .filter(|f| !f.ends_with(".disabled"))
            .map(|f| {
                let m = manifest(&dir.join(&f), loader);
                (f, m)
            })
            .collect();
        let mut removed = BTreeSet::from([name.to_string()]);
        let all: BTreeSet<_> = entries.values().flat_map(|m| m.ids.iter()).collect();
        loop {
            let provided: BTreeSet<_> = entries
                .iter()
                .filter(|(f, _)| !removed.contains(*f))
                .flat_map(|(_, m)| m.ids.iter())
                .collect();
            let next = entries
                .iter()
                .filter(|(f, m)| {
                    !removed.contains(*f)
                        && m.needs.iter().any(|ids| {
                            ids.iter().any(|i| all.contains(i))
                                && !ids.iter().any(|i| provided.contains(i))
                        })
                })
                .map(|(f, m)| (f.clone(), m.name.clone()))
                .collect::<Vec<_>>();
            if next.is_empty() {
                break;
            }
            for (f, label) in next {
                removed.insert(f.clone());
                affected.insert(f, label);
            }
        }
    }
    Ok(Toggle {
        name: name.into(),
        affected,
    })
}
pub fn apply(root: &Path, item: &Instance, seen: &Toggle) -> Result<()> {
    let _lock = crate::library::lock_instance(root, item, "请先退出游戏")?;
    let editor = super::history::Editor::open(root, item)?;
    ensure!(
        inspect(&editor.dir, item.loader, &seen.name)? == *seen,
        "模组依赖已变化，请重新检查"
    );
    let mut edits = super::history::Edits::new();
    for file in std::iter::once(&seen.name).chain(seen.affected.keys()) {
        let dest = file
            .strip_suffix(".disabled")
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{file}.disabled"));
        ensure!(
            !editor.dir.join(&dest).try_exists()?,
            "目标已有文件：{dest}"
        );
        edits.insert(dest, Some(editor.dir.join(file)));
        edits.insert(file.clone(), None);
    }
    let action = if seen.name.ends_with(".disabled") {
        "启用"
    } else {
        "停用"
    };
    editor
        .commit(&format!("{action}模组 · {}", seen.name), edits)
        .context("更改模组启用状态失败")
}
