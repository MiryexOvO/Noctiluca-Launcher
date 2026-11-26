mod legacy;
use crate::{
    download::Downloads,
    java,
    library::{Instance, Loader, atomic_write, safe_id},
};
use anyhow::{Context, Result, ensure};
use mc_launcher_core::{
    core::version::VersionJson,
    install::{client::read_version_json, vanilla::plan_vanilla_downloads},
    loader::{forge, neoforge},
    net::download::{Checksum, DownloadTask},
};
use std::{
    fs::{self, File},
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

pub fn install(
    root: &Path,
    shared: &Path,
    instance: &Instance,
    base: &VersionJson,
    downloads: &Downloads,
    pinned: Option<&str>,
) -> Result<VersionJson> {
    let mc = &instance.version;
    let (build, url, id) = match instance.loader {
        Loader::Forge => {
            let build = match pinned {
                Some(id) => format!(
                    "{}-{}",
                    mc,
                    id.strip_prefix(&format!("{mc}-forge-"))
                        .context("Forge 档案不匹配")?
                ),
                None if instance.loader_version.is_some() => {
                    format!("{mc}-{}", instance.loader_version.as_deref().unwrap())
                }
                None => forge::latest_for_minecraft(&forge::list_forge_versions()?, mc)?.to_owned(),
            };
            let id = forge::forge_installed_version_id(&build)?;
            let url = forge::installer_url(&build);
            (build, url, id)
        }
        Loader::NeoForge => {
            let build = match pinned {
                Some(id) => id
                    .strip_prefix("neoforge-")
                    .context("NeoForge 档案不匹配")?
                    .to_owned(),
                None if instance.loader_version.is_some() => {
                    instance.loader_version.clone().unwrap()
                }
                None => neoforge::latest_for_minecraft(&neoforge::list_neoforge_versions()?, mc)?
                    .to_owned(),
            };
            let id = neoforge::neoforge_installed_version_id(mc, &build);
            let url = neoforge::installer_url(&build);
            (build, url, id)
        }
        _ => anyhow::bail!("加载器不使用 Java 安装器"),
    };
    ensure!(safe_id(&id) && safe_id(&build), "加载器版本无效");
    if let Ok(profile) = read_version_json(shared, &id)
        && profile.id.as_ref() == Some(&id)
        && profile.inherits_from.as_ref() == Some(mc)
        && !missing_processor_output(&profile, shared)
    {
        return Ok(profile);
    }
    let java = java::select(
        shared,
        base,
        downloads,
        instance
            .java_path
            .as_deref()
            .or(crate::preferences::current().java.as_deref()),
    )?;
    downloads.batch(
        shared,
        "准备加载器基础文件",
        plan_vanilla_downloads(base, shared)?.tasks,
    )?;
    let checksum = downloads
        .http
        .get(format!("{url}.sha1"))
        .send()?
        .error_for_status()?
        .text()?;
    let sha1 = checksum
        .split_whitespace()
        .next()
        .context("加载器安装程序缺少校验值")?;
    ensure!(
        sha1.len() == 40 && sha1.bytes().all(|c| c.is_ascii_hexdigit()),
        "加载器安装程序校验值无效"
    );
    let jar = shared.join("installers").join(format!("{id}.jar"));
    downloads.batch(
        shared,
        "下载加载器安装程序",
        vec![DownloadTask {
            url,
            destination: jar.clone(),
            checksum: Some(Checksum::Sha1(sha1.to_ascii_lowercase())),
            label: build.clone(),
        }],
    )?;
    if instance.loader == Loader::Forge
        && mc == "1.12.2"
        && let Some(profile) = legacy::extract(&jar, shared, mc, &build, &id)?
    {
        return Ok(profile);
    }
    let profiles = shared.join("launcher_profiles.json");
    if !profiles.exists() {
        atomic_write(&profiles, br#"{"profiles":{}}"#)?;
    }
    let out = File::create(instance.path(root).join("loader-install.log"))?;
    let mut command = Command::new(java);
    java::hide_console(&mut command);
    for (scheme, var) in [("https", "HTTPS_PROXY"), ("http", "HTTP_PROXY")] {
        if let Some(proxy) = std::env::var(var)
            .ok()
            .and_then(|s| url::Url::parse(&s).ok())
            && proxy.scheme() == "http"
            && proxy.username().is_empty()
            && proxy.password().is_none()
            && let (Some(host), Some(port)) = (proxy.host_str(), proxy.port_or_known_default())
        {
            command
                .arg(format!("-D{scheme}.proxyHost={host}"))
                .arg(format!("-D{scheme}.proxyPort={port}"));
        }
    }
    (downloads.report)(format!("正在运行 {} 安装程序", instance.loader.label()));
    let mut child = command
        .arg("-Djava.net.useSystemProxies=true")
        .arg("-jar")
        .arg(jar)
        .arg("--installClient")
        .arg(shared)
        .current_dir(shared)
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .spawn()?;
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(
                status.success(),
                "加载器安装失败（{status}），请查看实例中的 loader-install.log"
            );
            break;
        }
        if let Err(cancelled) = downloads.check() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(cancelled);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let profile = read_version_json(shared, &id)?;
    ensure!(
        profile.inherits_from.as_ref() == Some(mc),
        "加载器安装结果不匹配"
    );
    Ok(profile)
}

pub fn missing_processor_output(profile: &VersionJson, shared: &Path) -> bool {
    profile
        .libraries
        .iter()
        .filter_map(|l| l.downloads.as_ref()?.artifact.as_ref())
        .filter(|a| a.url.is_empty())
        .any(|a| {
            let path = shared.join("libraries").join(&a.path);
            fs::metadata(path).map_or(true, |m| m.len() != a.size as u64)
        })
}
