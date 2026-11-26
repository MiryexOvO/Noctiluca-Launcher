use super::*;
use std::{io::Write, sync::atomic::Ordering};
use walkdir::WalkDir;

#[derive(Default)]
pub(crate) struct SupplyReport {
    pub matched: Vec<usize>,
    pub errors: usize,
    pub first_error: Option<String>,
}
impl SupplyReport {
    fn record_error(&mut self, message: String) {
        self.errors += 1;
        self.first_error.get_or_insert(message);
    }
}

fn hash_copy(mut input: File, mut output: impl Write, cancel: &AtomicBool) -> Result<String> {
    let mut sha1 = sha1::Sha1::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        ensure!(!cancel.load(Ordering::Relaxed), "已取消校验");
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        sha1.update(&buffer[..n]);
        output.write_all(&buffer[..n])?;
    }
    Ok(hex::encode(sha1.finalize()))
}

impl Preview {
    pub fn supply(&self, index: usize, source: &Path, cancel: &AtomicBool) -> Result<()> {
        let file = self.files.get(index).context("清单文件不存在")?;
        let input = File::open(source).context("无法读取所选文件")?;
        let mut copy = tempfile::NamedTempFile::new_in(&self.cache)?;
        let hash = hash_copy(input, &mut copy, cancel)?;
        ensure!(
            hash.eq_ignore_ascii_case(&file.hashes["sha1"]),
            "{} 与清单 SHA-1 不符，请选择指定版本",
            file.path
        );
        self.check_sha512(file, copy.path(), cancel)?;
        ensure!(!cancel.load(Ordering::Relaxed), "已取消校验");
        let saved = copy.persist(self.cache_path(index))?;
        self.cached_sizes.lock().unwrap()[index] = Some(saved.metadata()?.len());
        Ok(())
    }

    pub fn supply_batch(
        &self,
        paths: &[PathBuf],
        optional: &HashSet<usize>,
        cancel: &AtomicBool,
    ) -> SupplyReport {
        let mut targets: HashMap<String, Vec<usize>> = HashMap::new();
        let mut sizes = HashSet::new();
        let mut unknown_size = false;
        {
            let cached = self.cached_sizes.lock().unwrap();
            for (i, file) in self.files.iter().enumerate() {
                if cached[i].is_some() || (file.optional() && !optional.contains(&i)) {
                    continue;
                }
                targets
                    .entry(file.hashes["sha1"].to_ascii_lowercase())
                    .or_default()
                    .push(i);
                if let Some(size) = file.size {
                    sizes.insert(size);
                } else {
                    unknown_size = true;
                }
            }
        }
        let mut report = SupplyReport::default();
        let mut seen = HashSet::new();
        for root in paths {
            if targets.is_empty() || cancel.load(Ordering::Relaxed) {
                break;
            }
            let entries = WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| {
                    #[cfg(windows)]
                    {
                        use std::os::windows::fs::MetadataExt;
                        if e.depth() > 0 && e.file_type().is_dir() {
                            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                            return e.metadata().is_ok_and(|m| {
                                m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
                            });
                        }
                    }
                    true
                });
            for entry in entries {
                if targets.is_empty() || cancel.load(Ordering::Relaxed) {
                    break;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        report.record_error(e.to_string());
                        continue;
                    }
                };
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                if !seen.insert(path.to_owned()) {
                    continue;
                }
                let result = (|| -> Result<()> {
                    if entry.depth() != 0
                        && !unknown_size
                        && !sizes.contains(&entry.metadata()?.len())
                    {
                        return Ok(());
                    }
                    let hash = hash_copy(File::open(path)?, io::sink(), cancel)?;
                    if let Some(indices) = targets.get_mut(&hash) {
                        // A single download may satisfy several distinct manifest paths.
                        indices.retain(|&i| {
                            if cancel.load(Ordering::Relaxed) {
                                return true;
                            }
                            match self.supply(i, path, cancel) {
                                Ok(()) => {
                                    report.matched.push(i);
                                    false
                                }
                                Err(e) => {
                                    if !cancel.load(Ordering::Relaxed) {
                                        report.record_error(format!(
                                            "{} · {e:#}",
                                            self.files[i].path
                                        ));
                                    }
                                    true
                                }
                            }
                        });
                        if indices.is_empty() {
                            targets.remove(&hash);
                        }
                    }
                    Ok(())
                })();
                if let Err(e) = result
                    && !cancel.load(Ordering::Relaxed)
                {
                    report.record_error(format!("{} · {e:#}", path.display()));
                }
            }
        }
        report
    }
}

pub(crate) fn is_pack_archive(path: &Path) -> bool {
    File::open(path)
        .ok()
        .and_then(|file| ZipArchive::new(file).ok())
        .is_some_and(|mut zip| read_index(&mut zip).is_ok())
}
