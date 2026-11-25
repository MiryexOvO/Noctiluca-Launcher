use anyhow::Result;
use regex::Regex;
use std::{
    fs::File,
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
    let text = text.to_ascii_lowercase();
    for (needle, hint) in [
        (
            "can't be written",
            "游戏进程无法写入实例目录中的 JAR。请检查目录写入权限和 Windows 对该进程的限制。",
        ),
        (
            "readonlyfilesystemexception",
            "Java 将实例目录中的文件判定为只读，请检查实例目录和进程权限。",
        ),
        (
            "outofmemoryerror",
            "Java 堆内存不足，可尝试提高此实例的最大内存。",
        ),
        (
            "could not reserve enough space",
            "Java 无法分配所选内存，可降低内存或检查 Java 架构。",
        ),
        (
            "unsupportedclassversionerror",
            "当前 Java 版本过低，请检查自定义 Java 设置。",
        ),
        (
            "incompatible mod set",
            "模组依赖不兼容，加载器在游戏日志中列出了冲突项。",
        ),
        (
            "incompatible mods found",
            "加载器发现不兼容的模组，请核对模组版本及其依赖。",
        ),
        (
            "nosuchmethoderror",
            "库或模组之间的方法版本不匹配，请核对最近更换的模组。",
        ),
        (
            "could not find or load main class",
            "缺少游戏入口类，请运行安装 / 修复。",
        ),
        ("glfw error 65542", "OpenGL 初始化失败，请检查显卡驱动。"),
    ] {
        if text.contains(needle) {
            return Some(hint);
        }
    }
    None
}

pub fn redact(text: &str) -> String {
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)((?:--accessToken|refresh_token|access_token|authorization)\s*["'=:\s]+(?:Bearer\s+)?)[^\s,"'}]+"#).unwrap()
    });
    static JWT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b").unwrap()
    });
    JWT.replace_all(&TOKEN.replace_all(text, "${1}[已隐藏]"), "[已隐藏]")
        .into_owned()
}
