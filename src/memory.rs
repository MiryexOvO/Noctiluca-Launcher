use crate::library::{Instance, atomic_write};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Mode {
    Global,
    Auto,
    #[default]
    Custom,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Global {
    pub automatic: bool,
    pub max: u32,
    pub initial: u32,
}
impl Default for Global {
    fn default() -> Self {
        Self {
            automatic: true,
            max: 4096,
            initial: 512,
        }
    }
}
impl Global {
    pub fn read(root: &Path) -> Result<Self> {
        let file = root.join("global-memory.json");
        let g: Self = match fs::read(file) {
            Ok(b) => serde_json::from_slice(&b)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e.into()),
        };
        validate(g.initial, g.max)?;
        Ok(g)
    }
    pub fn save(self, root: &Path) -> Result<()> {
        validate(self.initial, self.max)?;
        atomic_write(
            &root.join("global-memory.json"),
            &serde_json::to_vec(&self)?,
        )
    }
}
pub fn validate(min: u32, max: u32) -> Result<()> {
    ensure!(
        (512..=32768).contains(&max) && (128..=max).contains(&min),
        "内存范围无效，请使初始内存小于或等于最大内存"
    );
    Ok(())
}
pub fn initial() -> u32 {
    512
}
pub struct Physical {
    pub total: u64,
    pub available: u64,
}
pub fn physical() -> Option<Physical> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // dwLength identifies the initialized structure expected by Windows.
        if unsafe { GlobalMemoryStatusEx(&mut status) } != 0 && status.ullTotalPhys > 0 {
            return Some(Physical {
                total: status.ullTotalPhys >> 20,
                available: status.ullAvailPhys >> 20,
            });
        }
    }
    None
}
pub fn effective(item: &Instance, global: Global, ram: Option<&Physical>) -> (u32, u32) {
    let auto = match item.memory_mode {
        Mode::Global => global.automatic,
        Mode::Auto => true,
        Mode::Custom => false,
    };
    if auto {
        let max = ram
            .map(|r| ((r.available / 2).clamp(512, 8192) as u32 / 128) * 128)
            .unwrap_or(2048);
        (512, max)
    } else if item.memory_mode == Mode::Global {
        (global.initial, global.max)
    } else {
        (item.initial_mib, item.memory_mib)
    }
}
