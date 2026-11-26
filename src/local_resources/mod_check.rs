use super::*;
use crate::mods::local::{self, LocalMod};
use std::collections::BTreeSet;

#[derive(Clone)]
pub(super) struct Previous {
    pub name: String,
    pub hash: String,
}
#[derive(Clone, Default)]
pub(crate) struct Check {
    pub(super) ids: BTreeSet<String>,
    pub(super) old: Option<Previous>,
    pub(super) output: String,
    pub note: String,
    pub blocked: bool,
}
fn overlaps(ids: &BTreeSet<String>, item: &LocalMod) -> bool {
    item.entries
        .iter()
        .any(|e| ids.contains(&e.id.to_ascii_lowercase()))
}
impl Check {
    pub(super) fn inspect(
        path: &Path,
        name: &str,
        dir: &Path,
        item: &Instance,
        installed: &[LocalMod],
    ) -> Result<Self> {
        let info = local::describe(path);
        let mut out = Self {
            ids: info
                .entries
                .iter()
                .map(|e| e.id.to_ascii_lowercase())
                .collect(),
            output: name.into(),
            ..Self::default()
        };
        let mut notes = vec![];
        match info.compatibility(item) {
            Ok(Some(s)) => notes.push(s),
            Ok(None) => {}
            Err(s) => {
                out.blocked = true;
                notes.push(s);
            }
        }
        let hits = installed
            .iter()
            .filter(|m| {
                overlaps(&out.ids, m)
                    || m.file
                        .strip_suffix(".disabled")
                        .unwrap_or(&m.file)
                        .eq_ignore_ascii_case(name)
            })
            .collect::<Vec<_>>();
        let incoming = LocalMod {
            file: name.into(),
            entries: info.entries,
        }
        .label();
        if !incoming.is_empty() {
            notes.push(format!("拖入版本  ·  {incoming}"));
        }
        match hits.as_slice() {
            [m] => {
                let name = &m.file;
                if m.disabled() {
                    out.output.push_str(".disabled");
                }
                out.old = Some(Previous {
                    name: name.clone(),
                    hash: fingerprint(dir, name)?.context("已装模组已变化，请重新检查")?,
                });
                notes.push(format!("现有版本  ·  {}  ({name})", m.label()));
                if m.disabled() {
                    notes.push("替换后继续停用".into());
                }
                if overlaps(&out.ids, m)
                    && m.entries
                        .iter()
                        .any(|e| !out.ids.contains(&e.id.to_ascii_lowercase()))
                {
                    out.blocked = true;
                    notes.push("原文件还提供其他模组，请先在模组管理中检查".into());
                }
            }
            [] => {}
            _ => {
                out.blocked = true;
                notes.push(format!(
                    "有多个匹配文件，请先整理：{}",
                    hits.iter()
                        .map(|m| m.file.as_str())
                        .collect::<Vec<_>>()
                        .join("、")
                ));
            }
        }
        out.note = notes.join("　/　");
        Ok(out)
    }
    pub(super) fn validate(
        &self,
        dir: &Path,
        inventory: &[LocalMod],
        source_hash: &str,
    ) -> Result<()> {
        ensure!(!self.blocked, "这个模组不能安装到当前实例");
        let base = self
            .output
            .strip_suffix(".disabled")
            .unwrap_or(&self.output);
        for m in inventory.iter().filter(|m| {
            overlaps(&self.ids, m)
                || m.file
                    .strip_suffix(".disabled")
                    .unwrap_or(&m.file)
                    .eq_ignore_ascii_case(base)
        }) {
            if self
                .old
                .as_ref()
                .is_some_and(|old| m.file.eq_ignore_ascii_case(&old.name))
            {
                continue;
            }
            ensure!(
                m.file.eq_ignore_ascii_case(&self.output)
                    && fingerprint(dir, &m.file)?.as_deref() == Some(source_hash),
                "模组列表已变化，请重新检查：{}",
                m.file
            );
        }
        if let Some(old) = &self.old {
            let hash = fingerprint(dir, &old.name)?;
            let installed = fingerprint(dir, &self.output)?.as_deref() == Some(source_hash);
            ensure!(
                if installed && !old.name.eq_ignore_ascii_case(&self.output) {
                    hash.is_none()
                } else {
                    hash.as_ref() == Some(&old.hash) || installed
                },
                "现有模组已变化，请重新拖入：{}",
                old.name
            );
        }
        Ok(())
    }
}
pub(super) fn move_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |p: &Path| {
        p.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<u16>>()
    };
    let a = wide(from);
    let b = wide(to);
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            a.as_ptr(),
            b.as_ptr(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
