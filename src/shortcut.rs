use crate::library::{self, Instance};
use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsString,
    fs,
    io::Cursor,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::RPC_E_CHANGED_MODE,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize, IPersistFile, STGM_READ,
        },
        UI::Shell::{
            FOLDERID_Desktop, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink,
        },
    },
    core::{HSTRING, Interface},
};

struct Com(bool);
impl Com {
    fn new() -> Result<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr != RPC_E_CHANGED_MODE {
            hr.ok()?;
        }
        Ok(Self(hr.is_ok()))
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}
fn icon(root: &Path, item: &Instance) -> Result<Option<PathBuf>> {
    let Some(bytes) = crate::instance_icon::load(root, &item.id) else {
        return Ok(None);
    };
    let source = image::load_from_memory(&bytes)?;
    let mut entries = Vec::new();
    for size in [32, 48, 256] {
        let mut buf = Cursor::new(Vec::new());
        source
            .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
            .write_to(&mut buf, image::ImageFormat::Png)?;
        entries.push((size, buf.into_inner()));
    }
    let mut ico = vec![0, 0, 1, 0, entries.len() as u8, 0];
    let mut offset = 6 + 16 * entries.len() as u32;
    for (size, data) in &entries {
        ico.extend([*size as u8, *size as u8, 0, 0, 1, 0, 32, 0]);
        ico.extend((data.len() as u32).to_le_bytes());
        ico.extend(offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in entries {
        ico.extend(data)
    }
    let path = item.path(root).join("shortcut.ico");
    library::atomic_write(&path, &ico)?;
    Ok(Some(path))
}
pub fn desktop(root: &Path, item: &Instance) -> Result<PathBuf> {
    let _com = Com::new()?;
    let desktop = unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None)?;
        let v = OsString::from_wide(p.as_wide());
        CoTaskMemFree(Some(p.0.cast()));
        PathBuf::from(v)
    };
    create_in(root, item, &desktop, &std::env::current_exe()?)
}
fn title(name: &str) -> String {
    let s = name
        .chars()
        .filter(|c| !c.is_control() && !r#"<>:"/\|?*"#.contains(*c))
        .take(75)
        .collect::<String>();
    let s = s.trim().trim_end_matches([' ', '.']);
    let head = s.split('.').next().unwrap_or_default().to_uppercase();
    if s.is_empty() {
        "Minecraft".into()
    } else if matches!(
        head.as_str(),
        "CON" | "NUL" | "PRN" | "AUX" | "CONOUT$" | "CONIN$"
    ) || ["COM", "LPT"].iter().any(|p| {
        head.strip_prefix(p)
            .is_some_and(|s| s.len() == 1 && s.chars().all(|v| v.is_ascii_digit()))
    }) {
        format!("Minecraft - {s}")
    } else {
        s.into()
    }
}
fn new_link() -> Result<IShellLinkW> {
    Ok(unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? })
}
fn same_link(path: &Path, args: &str, exe: &Path) -> bool {
    let check = || -> Result<bool> {
        let link = new_link()?;
        let persist: IPersistFile = link.cast()?;
        let mut argv = vec![0; 32768];
        let mut target = vec![0; 32768];
        unsafe {
            persist.Load(&HSTRING::from(path.as_os_str()), STGM_READ)?;
            link.GetArguments(&mut argv)?;
            link.GetPath(&mut target, std::ptr::null_mut(), 0)?;
        }
        let value = |s: &[u16]| {
            OsString::from_wide(&s[..s.iter().position(|v| *v == 0).unwrap_or(s.len())])
        };
        Ok(value(&argv) == args && Path::new(&value(&target)) == exe)
    };
    check().unwrap_or(false)
}
pub(crate) fn create_in(root: &Path, item: &Instance, dir: &Path, exe: &Path) -> Result<PathBuf> {
    ensure!(
        library::safe_id(&item.id) && root.is_absolute() && exe.is_absolute(),
        "快捷方式的路径或实例编号无效"
    );
    let _com = Com::new()?;
    let link = new_link()?;
    let data = root.to_str().context("数据目录编码无效")?;
    let trailing = data.len() - data.trim_end_matches('\\').len();
    let args = format!(
        "--launch {} --data-dir \"{}{}\"",
        item.id,
        data,
        "\\".repeat(trailing)
    );
    let image = icon(root, item)?.unwrap_or_else(|| exe.to_owned());
    let persist: IPersistFile = link.cast()?;
    let staging = tempfile::tempdir_in(dir)?;
    let path = staging.path().join("Noctiluca.lnk");
    unsafe {
        link.SetPath(&HSTRING::from(exe.as_os_str()))?;
        link.SetArguments(&HSTRING::from(&args))?;
        link.SetWorkingDirectory(&HSTRING::from(
            exe.parent().context("找不到启动器目录")?.as_os_str(),
        ))?;
        link.SetDescription(&HSTRING::from(format!("用 Noctiluca 启动 {}", item.name)))?;
        link.SetIconLocation(&HSTRING::from(image.as_os_str()), 0)?;
        persist.Save(&HSTRING::from(path.as_os_str()), true)?;
    }
    let stem = title(&item.name);
    for n in 1.. {
        let name = if n == 1 {
            format!("{stem}.lnk")
        } else {
            format!("{stem} ({n}).lnk")
        };
        let dest = dir.join(name);
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        std::io::copy(&mut fs::File::open(&path)?, tmp.as_file_mut())?;
        match tmp.persist_noclobber(&dest) {
            Ok(_) => return Ok(dest),
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if same_link(&dest, &args, exe) {
                    e.file.persist(&dest)?;
                    return Ok(dest);
                }
            }
            Err(e) => return Err(e.error.into()),
        }
    }
    unreachable!()
}
