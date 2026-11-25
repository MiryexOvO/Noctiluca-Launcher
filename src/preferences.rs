use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, RwLock},
};
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub files: u8,
    pub catalog: u8,
    pub protocol: u8,
    pub connections: u8,
    pub streams: u8,
    pub limit: u16,
    pub dependencies: bool,
    pub hide_quilt: bool,
    pub select_new: bool,
    pub community: u8,
    pub visibility: u8,
    pub priority: u8,
    pub ip: u8,
    pub size: u8,
    pub game_title: u8,
    pub brand: String,
    pub accent: u8,
    pub server: String,
    pub join_server: bool,
    pub java: Option<PathBuf>,
    pub isolated: u8,
    pub language: u8,
    pub sync_lang: bool,
    pub stable: bool,
    pub snapshots: bool,
    pub clipboard: bool,
    pub name_style: u8,
    pub quick: u8,
    pub file_style: u8,
    pub authlib: bool,
}
impl Default for Prefs {
    fn default() -> Self {
        Self {
            accent: 0,
            server: String::new(),
            join_server: false,
            files: 0,
            catalog: 0,
            protocol: 0,
            connections: 8,
            streams: 1,
            limit: 0,
            dependencies: true,
            hide_quilt: false,
            select_new: true,
            community: 0,
            visibility: 0,
            priority: 1,
            ip: 0,
            size: 0,
            game_title: 0,
            brand: "Noctiluca".into(),
            java: None,
            isolated: 0,
            language: 0,
            sync_lang: false,
            stable: false,
            snapshots: false,
            clipboard: false,
            name_style: 0,
            quick: 0,
            file_style: 0,
            authlib: false,
        }
    }
}
static CURRENT: LazyLock<RwLock<Prefs>> = LazyLock::new(|| RwLock::new(Prefs::default()));
pub fn current() -> Prefs {
    CURRENT.read().unwrap().clone()
}
impl Prefs {
    fn validate(&self) -> Result<()> {
        ensure!(
            (1..=64).contains(&self.connections)
                && (1..=8).contains(&self.streams)
                && self.limit <= 1024,
            "下载参数超出范围"
        );
        ensure!(self.brand.chars().count() <= 80, "自定义信息过长");
        Ok(())
    }
    pub fn init(root: &Path) -> Result<()> {
        let p = match fs::read(root.join("settings.json")) {
            Ok(b) => serde_json::from_slice(&b)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e.into()),
        };
        p.validate()?;
        *CURRENT.write().unwrap() = p;
        Ok(())
    }
    pub fn save(self, root: &Path) -> Result<()> {
        self.validate()?;
        crate::library::atomic_write(
            &root.join("settings.json"),
            &serde_json::to_vec_pretty(&self)?,
        )?;
        *CURRENT.write().unwrap() = self;
        Ok(())
    }
    pub fn resolution(&self) -> Option<(u32, u32)> {
        match self.size {
            1 => Some((1280, 720)),
            2 => Some((1600, 900)),
            3 => Some((1920, 1080)),
            _ => None,
        }
    }
    pub fn game_lang(&self) -> &'static str {
        ["zh_cn", "en_us", "ja_jp"]
            .get(self.language as usize)
            .unwrap_or(&"zh_cn")
    }
}
pub fn set_language(dir: &Path, p: &Prefs) -> Result<()> {
    if !p.sync_lang {
        return Ok(());
    }
    let path = dir.join("options.txt");
    let old = match fs::read_to_string(&path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let desired = format!("lang:{}", p.game_lang());
    if old.lines().any(|s| s == desired) {
        return Ok(());
    }
    let mut lines = old
        .lines()
        .filter(|s| !s.starts_with("lang:"))
        .collect::<Vec<_>>();
    lines.push(&desired);
    crate::library::atomic_write(&path, format!("{}\n", lines.join("\n")).as_bytes())
}
pub fn mirror(input: &str) -> Option<String> {
    let u = url::Url::parse(input).ok()?;
    let path = u.path();
    let suffix = match u.host_str()? {
        "resources.download.minecraft.net" => format!("/assets{path}"),
        "libraries.minecraft.net" => format!("/maven{path}"),
        "piston-meta.mojang.com"
        | "launchermeta.mojang.com"
        | "piston-data.mojang.com"
        | "launcher.mojang.com" => path.to_owned(),
        "maven.minecraftforge.net" => format!("/maven{path}"),
        _ => return None,
    };
    Some(format!("https://bmclapi2.bangbang93.com{suffix}"))
}
pub fn sources(url: &str, mode: u8) -> Vec<String> {
    match (mode, mirror(url)) {
        (1, Some(m)) => vec![m, url.into()],
        (2, _) | (_, None) => vec![url.into()],
        (_, Some(m)) => vec![url.into(), m],
    }
}
pub fn clipboard_link(text: &str) -> Option<url::Url> {
    let u = url::Url::parse(text.trim()).ok()?;
    if u.scheme() != "https"
        || u.port().is_some()
        || !u.username().is_empty()
        || u.password().is_some()
    {
        return None;
    }
    match u.host_str()? {
        "modrinth.com"
            if ["/mod/", "/modpack/", "/shader/", "/resourcepack/"]
                .iter()
                .any(|s| u.path().starts_with(s)) =>
        {
            Some(u)
        }
        "www.curseforge.com" | "curseforge.com" if u.path().starts_with("/minecraft/") => Some(u),
        _ => None,
    }
}
pub fn check_release() -> Result<String> {
    let response = crate::download::client()?
        .get("https://api.github.com/repos/MiryexOvO/Noctiluca-Launcher/releases/latest")
        .send()?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok("仓库尚无可用的公开发行版".into());
    }
    let data: serde_json::Value = response.error_for_status()?.json()?;
    let tag = data["tag_name"].as_str().context("发行版缺少版本号")?;
    Ok(format!(
        "当前 {} · 最新公开发行版 {tag}",
        env!("CARGO_PKG_VERSION")
    ))
}

pub fn set_window_title(pid: u32, title: String) {
    std::thread::spawn(move || {
        use windows_sys::Win32::{
            Foundation::{HWND, LPARAM},
            UI::WindowsAndMessaging::{
                EnumWindows, GetWindowThreadProcessId, IsWindowVisible, SetWindowTextW,
            },
        };
        struct Target {
            pid: u32,
            title: Vec<u16>,
            found: bool,
        }
        unsafe extern "system" fn visit(w: HWND, data: LPARAM) -> i32 {
            let target = unsafe { &mut *(data as *mut Target) };
            let mut owner = 0;
            unsafe {
                GetWindowThreadProcessId(w, &mut owner);
            }
            if owner == target.pid && unsafe { IsWindowVisible(w) } != 0 {
                target.found = unsafe { SetWindowTextW(w, target.title.as_ptr()) } != 0;
                if target.found {
                    return 0;
                }
            }
            1
        }
        let mut target = Target {
            pid,
            title: title.encode_utf16().chain(Some(0)).collect(),
            found: false,
        };
        for _ in 0..120 {
            unsafe {
                EnumWindows(Some(visit), (&raw mut target) as LPARAM);
            }
            if target.found {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

pub fn server(address: &str) -> Result<(String, u16)> {
    ensure!(
        !address.trim().is_empty()
            && !address.contains(['/', '\\', '@', '?', '#'])
            && !address.chars().any(char::is_whitespace),
        "服务器地址无效"
    );
    let url = url::Url::parse(&format!("tcp://{address}"))?;
    let host = url.host_str().context("请输入域名或 IP")?.to_owned();
    let port = url.port().unwrap_or(25565);
    ensure!(port > 0, "端口无效");
    Ok((host, port))
}
