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
    let builder = Client::builder();
    let builder = if crate::preferences::current().protocol == 1 {
        builder.http1_only()
    } else {
        builder
    };
    builder
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
            pool: rayon::ThreadPoolBuilder::new()
                .num_threads(usize::from(crate::preferences::current().connections))
                .build()?,
        })
    }

    pub fn check(&self) -> Result<()> {
        ensure!(!self.cancel.load(Ordering::Relaxed), "已取消安装");
        Ok(())
    }

    pub fn batch(&self, root: &Path, title: &str, tasks: Vec<DownloadTask>) -> Result<()> {
        self.batch_with(root, title, tasks, |_| {}, |_| Ok(()))
    }

    pub fn batch_with(
        &self,
        root: &Path,
        title: &str,
        tasks: Vec<DownloadTask>,
        started: impl Fn(&DownloadTask) + Sync,
        completed: impl Fn(&DownloadTask) -> Result<()> + Sync,
    ) -> Result<()> {
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
        let progress = Mutex::new((0usize, None::<Instant>));
        self.pool.install(|| {
            unique.par_iter().try_for_each(|task| -> Result<()> {
                self.check()?;
                started(task);
                {
                    let mut state = progress.lock().unwrap();
                    if state
                        .1
                        .is_none_or(|last| last.elapsed() >= Duration::from_millis(350))
                    {
                        (self.report)(format!("{title} · {} · {} / {total}", task.label, state.0));
                        state.1 = Some(Instant::now());
                    }
                }
                self.file(root, task)
                    .and_then(|()| completed(task))
                    .with_context(|| FileFailure::new(title, &task.label))?;
                let mut state = progress.lock().unwrap();
                state.0 += 1;
                if state.0 == total
                    || state
                        .1
                        .is_none_or(|last| last.elapsed() >= Duration::from_millis(350))
                {
                    (self.report)(format!("{title} · {} / {total}", state.0));
                    state.1 = Some(Instant::now());
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
        let prefs = crate::preferences::current();
        let urls = if task.checksum.is_some() {
            crate::preferences::sources(&task.url, prefs.files)
        } else {
            vec![task.url.clone()]
        };
        for attempt in 1..=3 {
            self.check()?;
            let result = (|| -> Result<()> {
                let source = &urls[((attempt - 1) as usize).min(urls.len() - 1)];
                let mut tmp = NamedTempFile::new_in(dir)?;
                let bytes = transfer(self, source, &mut tmp, &prefs)?;
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

fn copy_limited(
    d: &Downloads,
    input: &mut impl io::Read,
    out: &mut impl io::Write,
    limit: u16,
) -> Result<u64> {
    static NEXT: std::sync::LazyLock<Mutex<Instant>> =
        std::sync::LazyLock::new(|| Mutex::new(Instant::now()));
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0;
    loop {
        d.check()?;
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if limit > 0 {
            let delay = {
                let mut next = NEXT.lock().unwrap();
                let now = Instant::now();
                let begin = (*next).max(now);
                *next =
                    begin + Duration::from_secs_f64(n as f64 / (f64::from(limit) * 1024. * 1024.));
                begin.saturating_duration_since(now)
            };
            std::thread::sleep(delay);
        }
        out.write_all(&buf[..n])?;
        total += n as u64;
    }
    Ok(total)
}
fn transfer(
    d: &Downloads,
    url: &str,
    out: &mut NamedTempFile,
    prefs: &crate::preferences::Prefs,
) -> Result<u64> {
    use std::io::{Seek, SeekFrom};
    let streams = usize::from(prefs.streams.min(prefs.connections));
    if streams > 1
        && let Ok(head) = d.http.head(url).send()
        && head.status().is_success()
        && head
            .headers()
            .get("accept-ranges")
            .and_then(|h| h.to_str().ok())
            == Some("bytes")
        && let Some(size) = head
            .headers()
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
        && size >= 2 * 1024 * 1024
    {
        let chunk = size.div_ceil(streams as u64);
        let tmp = out.path().parent().context("下载目录无效")?;
        let result = d.pool.install(|| {
            (0..streams)
                .into_par_iter()
                .map(|i| -> Result<NamedTempFile> {
                    d.check()?;
                    let from = chunk * i as u64;
                    let to = (from + chunk).min(size) - 1;
                    let mut response = d
                        .http
                        .get(url)
                        .header("Range", format!("bytes={from}-{to}"))
                        .send()?
                        .error_for_status()?;
                    ensure!(
                        response.status() == reqwest::StatusCode::PARTIAL_CONTENT
                            && response
                                .headers()
                                .get("content-range")
                                .and_then(|v| v.to_str().ok())
                                == Some(&format!("bytes {from}-{to}/{size}")),
                        "服务器未接受分段请求"
                    );
                    let mut part = NamedTempFile::new_in(tmp)?;
                    ensure!(
                        copy_limited(d, &mut response, &mut part, prefs.limit)? == to - from + 1,
                        "分段长度错误"
                    );
                    part.seek(SeekFrom::Start(0))?;
                    Ok(part)
                })
                .collect::<Result<Vec<_>>>()
        });
        if let Ok(parts) = result {
            for mut p in parts {
                d.check()?;
                io::copy(&mut p, out)?;
            }
            return Ok(size);
        }
    }
    d.check()?;
    let mut response = d.http.get(url).send()?.error_for_status()?;
    let length = response.content_length();
    let count = copy_limited(d, &mut response, out, prefs.limit)?;
    ensure!(length.is_none_or(|v| v == count), "下载文件不完整");
    Ok(count)
}
