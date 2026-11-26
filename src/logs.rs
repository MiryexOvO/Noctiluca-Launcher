mod issue;
use anyhow::{Context, Result};
pub(crate) use issue::{Action, Issue};
use regex::Regex;
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::LazyLock,
};

pub fn tail(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let start = file.metadata()?.len().saturating_sub(32 * 1024);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(32 * 1024).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if start == 0 {
        text.as_ref()
    } else {
        text.split_once('\n').map_or(text.as_ref(), |(_, s)| s)
    };
    Ok(text
        .lines()
        .rev()
        .take(100)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn diagnosis(text: &str) -> Option<&'static str> {
    known_failure(text).map(|(hint, _)| hint)
}

fn known_failure(text: &str) -> Option<(&'static str, Option<Action>)> {
    let lower = text.to_ascii_lowercase();
    [
        (
            "can't be written",
            "游戏进程无法写入实例目录中的 JAR。请检查目录写入权限和 Windows 对该进程的限制。",
            None,
        ),
        (
            "readonlyfilesystemexception",
            "Java 将实例目录中的文件判定为只读，请检查实例目录和进程权限。",
            None,
        ),
        (
            "outofmemoryerror",
            "Java 堆内存不足，可尝试提高此实例的最大内存。",
            Some(Action::Memory),
        ),
        (
            "could not reserve enough space",
            "Java 无法分配所选内存，可降低内存或检查 Java 架构。",
            Some(Action::Memory),
        ),
        (
            "unsupportedclassversionerror",
            "当前 Java 版本过低，请检查自定义 Java 设置。",
            Some(Action::Java),
        ),
        (
            "incompatible mod set",
            "模组依赖不兼容，加载器在游戏日志中列出了冲突项。",
            Some(Action::Mods),
        ),
        (
            "incompatible mods found",
            "加载器发现不兼容的模组，请核对模组版本及其依赖。",
            Some(Action::Mods),
        ),
        (
            "nosuchmethoderror",
            "库或模组之间的方法版本不匹配，请核对最近更换的模组。",
            Some(Action::Mods),
        ),
        (
            "could not find or load main class",
            "缺少游戏入口类，请运行安装 / 修复。",
            Some(Action::Repair),
        ),
        (
            "glfw error 65542",
            "OpenGL 初始化失败，请检查显卡驱动。",
            None,
        ),
    ]
    .into_iter()
    .find(|(needle, _, _)| lower.contains(needle))
    .map(|(_, hint, action)| (hint, action))
}

pub fn redact(text: &str) -> String {
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)((?:--accessToken|--clientId|--xuid|refresh_token|access_token|clientToken|password|authorization)\s*["'=:\s]+(?:Bearer\s+)?)[^\s,"'}]+"#).unwrap()
    });
    static JWT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b").unwrap()
    });
    JWT.replace_all(&TOKEN.replace_all(text, "${1}[已隐藏]"), "[已隐藏]")
        .into_owned()
}

fn excerpt(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let start = file.metadata()?.len().saturating_sub(2 * 1024 * 1024);
    file.seek(SeekFrom::Start(start))?;
    let mut data = Vec::new();
    file.take(2 * 1024 * 1024).read_to_end(&mut data)?;
    Ok(String::from_utf8_lossy(&data).into_owned())
}
fn sources(
    root: &Path,
    item: &crate::library::Instance,
) -> Result<Vec<(&'static str, std::path::PathBuf)>> {
    let game = item.game_dir(root)?;
    let dir = item.path(root);
    let mut paths = vec![
        ("game-output.log", dir.join("last-launch.log")),
        ("latest.log", game.join("logs/latest.log")),
        ("launcher.log", dir.join("launcher.log")),
        ("loader.log", dir.join("loader-install.log")),
    ];
    if let Ok(entries) = fs::read_dir(game.join("crash-reports")) {
        let recent = entries
            .flatten()
            .filter(|e| {
                e.file_type().is_ok_and(|t| t.is_file())
                    && e.file_name().to_string_lossy().ends_with(".txt")
            })
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .max_by_key(|v| v.0);
        if let Some((_, p)) = recent {
            paths.push(("crash-report.txt", p));
        }
    }
    Ok(paths)
}
pub fn cause(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let lower = line.to_lowercase();
        if [
            " requires ",
            " depends on ",
            "is incompatible with",
            "incompatible with",
            " is missing",
            " is not installed",
            "failure message:",
            "mod file:",
            "suspected mod",
            "mandatory dependencies",
            "requested by:",
            "actual version:",
        ]
        .iter()
        .any(|s| lower.contains(s))
        {
            let v = line.trim().chars().take(420).collect::<String>();
            if !lines.contains(&v) {
                lines.push(v);
            }
            if lines.len() >= 6 {
                break;
            }
        }
    }
    let hint = diagnosis(text).unwrap_or("请检查游戏日志和崩溃报告。");
    if lines.is_empty() {
        hint.to_owned()
    } else {
        format!("{hint}\n{}", redact(&lines.join("\n")))
    }
}
pub fn export(root: &Path, item: &crate::library::Instance, path: &Path) -> Result<()> {
    use std::io::Write;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().context("请选择保存位置")?)?;
    let mut zip = zip::ZipWriter::new(temp.as_file_mut());
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut count = 0;
    let mut diagnosis = String::new();
    for (label, path) in sources(root, item)? {
        if !path.is_file() {
            continue;
        }
        let mut content = redact(&excerpt(&path)?);
        if let Ok(profile) = std::env::var("USERPROFILE")
            && !profile.is_empty()
        {
            content = content.replace(&profile, "[用户目录]");
        }
        if label != "launcher.log" && label != "loader.log" {
            diagnosis.push_str(&cause(&content));
            diagnosis.push('\n');
        }
        zip.start_file(label, opts)?;
        zip.write_all(content.as_bytes())?;
        count += 1;
    }
    anyhow::ensure!(count > 0, "此实例还没有可导出的日志");
    zip.start_file("diagnosis.txt", opts)?;
    write!(
        zip,
        "Minecraft {} · {}\n{}",
        item.version,
        item.loader.label(),
        diagnosis
    )?;
    zip.finish()?;
    temp.persist(path)?;
    Ok(())
}

pub fn launch_failure(
    root: &Path,
    item: &crate::library::Instance,
    started: std::time::SystemTime,
) -> Issue {
    let mut text = String::new();
    for (label, p) in sources(root, item).unwrap_or_default().into_iter().rev() {
        if !matches!(label, "latest.log" | "crash-report.txt" | "game-output.log") {
            continue;
        }
        if label != "game-output.log"
            && !fs::metadata(&p)
                .and_then(|m| m.modified())
                .is_ok_and(|m| m >= started)
        {
            continue;
        }
        if let Ok(s) = excerpt(&p) {
            text.push_str(&s);
            text.push('\n');
        }
    }
    Issue::from_log(&text)
}
