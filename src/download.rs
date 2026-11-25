use std::{
    collections::HashSet,
    fs, io,
    path::{Component, Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use mc_launcher_core::{
    io::hash::sha1_file,
    net::download::{Checksum, DownloadTask, should_skip_existing},
};
use rayon::prelude::*;
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

pub fn client() -> Result<Client> {
    Ok(client_builder().build()?)
}

pub fn client_builder() -> reqwest::blocking::ClientBuilder {
    Client::builder()
        .user_agent(format!("{}/{}", crate::APP_NAME, env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
}

pub struct Downloads<'a> {
    pub http: Client,
    pub cancel: &'a AtomicBool,
    pub cache_source: Option<PathBuf>,
    pub report: &'a (dyn Fn(String) + Sync),
    pool: rayon::ThreadPool,
}

#[derive(Debug)]
pub struct FileFailure {
    pub label: String,
    group: String,
}
impl FileFailure {
    pub fn new(group: &str, label: &str) -> Self {
        Self {
            group: group.into(),
            label: label.into(),
        }
    }
}
impl std::fmt::Display for FileFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} · {}", self.group, self.label)
    }
}

impl<'a> Downloads<'a> {
    pub fn new(cancel: &'a AtomicBool, report: &'a (dyn Fn(String) + Sync)) -> Result<Self> {
        Ok(Self {
            http: client()?,
            cancel,
            cache_source: None,
            report,
            pool: rayon::ThreadPoolBuilder::new().num_threads(8).build()?,
        })
    }

    pub fn check(&self) -> Result<()> {
        ensure!(!self.cancel.load(Ordering::Relaxed), "已取消安装");
        Ok(())
    }

    pub fn batch(&self, root: &Path, title: &str, tasks: Vec<DownloadTask>) -> Result<()> {
        self.check()?;
        let mut seen = HashSet::new();
        let mut unique = Vec::new();
        for task in tasks {
            let relative = task
                .destination
                .strip_prefix(root)
                .context("文件超出安装目录")?;
            ensure!(
                relative
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
                "文件路径无效"
            );
            if seen.insert(task.destination.clone()) {
                unique.push(task)
            }
        }
        let total = unique.len();
        (self.report)(format!("{title} · 0 / {total}"));
        let progress = Mutex::new((0usize, Instant::now()));
        self.pool.install(|| {
            unique.par_iter().try_for_each(|task| -> Result<()> {
                self.check()?;
                self.file(root, task)
                    .with_context(|| FileFailure::new(title, &task.label))?;
                let mut state = progress.lock().unwrap();
                state.0 += 1;
                if state.0 == total || state.1.elapsed() >= Duration::from_millis(350) {
                    (self.report)(format!("{title} · {} / {total}", state.0));
                    state.1 = Instant::now();
                }
                Ok(())
            })
        })
    }

    fn file(&self, root: &Path, task: &DownloadTask) -> Result<()> {
        if let Some(Checksum::Sha256(expected)) = &task.checksum
            && task.destination.is_file()
            && hex::encode(Sha256::digest(fs::read(&task.destination)?))
                .eq_ignore_ascii_case(expected)
        {
            return Ok(());
        }
        if should_skip_existing(task)? {
            return Ok(());
        }
        let dir = task.destination.parent().context("缺少文件目录")?;
        fs::create_dir_all(dir)?;
        if let (Some(source), Some(Checksum::Sha1(expected))) = (&self.cache_source, &task.checksum)
        {
            let relative = task.destination.strip_prefix(root)?;
            if ["assets", "libraries", "versions"]
                .iter()
                .any(|part| relative.starts_with(part))
            {
                let candidate = source.join(relative);
                if let Ok(mut input) = fs::File::open(candidate) {
                    let mut copy = NamedTempFile::new_in(dir)?;
                    if io::copy(&mut input, &mut copy).is_ok()
                        && sha1_file(copy.path())?.eq_ignore_ascii_case(expected)
                    {
                        self.check()?;
                        copy.persist(&task.destination)?;
                        return Ok(());
                    }
                }
            }
        }
        for attempt in 1..=3 {
            self.check()?;
            let result = (|| -> Result<()> {
                let mut response = self.http.get(&task.url).send()?.error_for_status()?;
                let mut tmp = NamedTempFile::new_in(dir)?;
                let expected_length = response.content_length();
                let bytes = io::copy(&mut response, &mut tmp)?;
                if let Some(length) = expected_length {
                    ensure!(bytes == length, "下载文件不完整");
                }
                match &task.checksum {
                    Some(Checksum::Sha1(expected)) => ensure!(
                        sha1_file(tmp.path())?.eq_ignore_ascii_case(expected),
                        "SHA-1 校验失败"
                    ),
                    Some(Checksum::Sha256(expected)) => ensure!(
                        hex::encode(Sha256::digest(fs::read(tmp.path())?))
                            .eq_ignore_ascii_case(expected),
                        "SHA-256 校验失败"
                    ),
                    None => ensure!(bytes > 0, "下载文件为空"),
                }
                self.check()?;
                tmp.persist(&task.destination)?;
                Ok(())
            })();
            match result {
                Ok(()) => return Ok(()),
                Err(err) if attempt == 3 => return Err(err),
                Err(_) => std::thread::sleep(Duration::from_millis(350 * attempt)),
            }
        }
        unreachable!()
    }
}
