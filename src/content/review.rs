use super::*;
use std::collections::{BTreeSet, HashMap};

pub(super) fn indexed(files: &BTreeMap<String, String>) -> BTreeMap<String, (&String, &String)> {
    files
        .iter()
        .map(|(name, hash)| (name.to_lowercase(), (name, hash)))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Conflict {
    base: Option<String>,
    local: Option<String>,
    incoming: Option<String>,
}
impl Conflict {
    pub fn note(&self) -> &'static str {
        if self.incoming.is_none() {
            "新版已移除"
        } else if self.local.is_none() {
            "你已删除"
        } else if self.base.is_none() {
            "与本地文件同名"
        } else {
            "你改过此文件"
        }
    }
}
pub(crate) type Choices = BTreeMap<String, Conflict>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Review {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub conflicts: Choices,
    pub legacy: bool,
}
fn is_config(rel: &str) -> bool {
    rel.split_once('/').is_some_and(|(head, _)| {
        head.eq_ignore_ascii_case("config") || head.eq_ignore_ascii_case("defaultconfigs")
    })
}
fn is_mod(rel: &str) -> bool {
    rel.to_ascii_lowercase().starts_with("mods/") && rel.to_ascii_lowercase().ends_with(".jar")
}
pub(crate) fn relevant(path: &str) -> bool {
    is_config(path) || is_mod(path)
}
// Only pair unique versioned filenames. Ambiguous names stay separate.
fn mod_key(rel: &str) -> Option<String> {
    static VERSION: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^(.+?)[_-](?:v|mc)?\d+(?:\.\d+)+(?:[.+_-]|$)").unwrap()
    });
    Some(VERSION.captures(rel.strip_suffix(".jar")?)?[1].to_lowercase())
}
impl Review {
    pub fn build(
        game: &Path,
        old: Option<&PackState>,
        next: &BTreeMap<String, String>,
    ) -> Result<Self> {
        let empty = BTreeMap::new();
        let prev = indexed(old.map(|s| &s.files).unwrap_or(&empty));
        let next = indexed(next);
        let mut out = Self {
            legacy: old.is_none(),
            ..Self::default()
        };
        let paths = prev.keys().chain(next.keys()).collect::<BTreeSet<_>>();
        for key in paths {
            let before = prev.get(key);
            let after = next.get(key);
            let rel = after.or(before).unwrap().0;
            let base = before.map(|v| v.1);
            let incoming = after.map(|v| v.1);
            if is_mod(rel) && base != incoming {
                match (base, incoming) {
                    (None, Some(_)) => out.added.push(rel.clone()),
                    (Some(_), None) => out.removed.push(rel.clone()),
                    _ => out.changed.push(rel.clone()),
                }
            }
            if is_config(rel) {
                let p = game.join(packs::relative(rel)?);
                let local = match fs::metadata(&p) {
                    Ok(m) if m.is_file() => Some(sha1_file(&p)?),
                    Ok(_) => bail!("配置路径不是普通文件：{rel}"),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                    Err(e) => return Err(e.into()),
                };
                if local.as_ref() != base && local.as_ref() != incoming {
                    out.conflicts.insert(
                        rel.clone(),
                        Conflict {
                            base: base.cloned(),
                            local,
                            incoming: incoming.cloned(),
                        },
                    );
                }
            }
        }
        let mut pairs: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
        for name in &out.added {
            if let Some(key) = mod_key(name) {
                pairs.entry(key).or_default().0.push(name.clone());
            }
        }
        for name in &out.removed {
            if let Some(key) = mod_key(name) {
                pairs.entry(key).or_default().1.push(name.clone());
            }
        }
        for (added, removed) in pairs.into_values() {
            if let ([a], [r]) = (added.as_slice(), removed.as_slice()) {
                out.added.retain(|v| v != a);
                out.removed.retain(|v| v != r);
                out.changed.push(format!("{r} → {a}"));
            }
        }
        out.changed.sort();
        Ok(out)
    }
}
