use crate::{
    accounts::{Kind, Player},
    download,
    library::atomic_write,
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat, imageops};
use sha2::{Digest, Sha256};
use std::{
    io::{Cursor, Read},
    path::{Path, PathBuf},
};
use url::Url;
use uuid::Uuid;
pub fn cache(root: &Path, p: &Player) -> PathBuf {
    let key = format!("{}:{}", p.key(), p.skin.as_deref().unwrap_or(""));
    root.join("avatars").join(format!(
        "{}.png",
        hex::encode(Sha256::digest(key.as_bytes()))
    ))
}
fn portrait(bytes: &[u8]) -> Result<Vec<u8>> {
    let reader = image::ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    ensure!(
        matches!(reader.into_dimensions()?, (64, 32) | (64, 64)),
        "皮肤尺寸无效"
    );
    let skin = image::load_from_memory_with_format(bytes, ImageFormat::Png)?.into_rgba8();
    let mut face = imageops::crop_imm(&skin, 8, 8, 8, 8).to_image();
    let hat = imageops::crop_imm(&skin, 40, 8, 8, 8).to_image();
    if skin.height() == 64 || hat.pixels().any(|p| p[3] < 255) {
        imageops::overlay(&mut face, &hat, 0, 0);
    }
    let mut result = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(imageops::resize(
        &face,
        64,
        64,
        imageops::FilterType::Nearest,
    ))
    .write_to(&mut result, ImageFormat::Png)?;
    Ok(result.into_inner())
}
pub fn local(root: &Path, p: &Player, file: &Path) -> Result<crate::accounts::Accounts> {
    ensure!(
        p.kind == Kind::Offline && std::fs::metadata(file)?.len() <= 1024 * 1024,
        "请选择不超过 1 MiB 的离线角色皮肤 PNG"
    );
    let bytes = std::fs::read(file)?;
    let mut p = p.clone();
    p.skin = Some(format!("local:{}", hex::encode(Sha256::digest(&bytes))));
    atomic_write(&cache(root, &p), &portrait(&bytes)?)?;
    crate::accounts::add(root, p, None)
}
pub fn fetch(root: &Path, p: &Player) -> Result<Option<PathBuf>> {
    let path = cache(root, p);
    if path.is_file() {
        return Ok(Some(path));
    }
    if p.kind == Kind::Offline {
        return Ok(None);
    }
    let http = download::client_builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let id = Uuid::parse_str(&p.id)?.simple().to_string();
    let url = if let Some(s) = &p.skin {
        s.clone()
    } else {
        let url = if p.kind == Kind::Microsoft {
            format!("https://sessionserver.mojang.com/session/minecraft/profile/{id}")
        } else {
            format!("{}sessionserver/session/minecraft/profile/{id}", p.client)
        };
        let profile: serde_json::Value = http.get(url).send()?.error_for_status()?.json()?;
        let prop = profile["properties"]
            .as_array()
            .and_then(|a| a.iter().find(|p| p["name"] == "textures"))
            .and_then(|p| p["value"].as_str())
            .context("此角色没有公开皮肤")?;
        let data: serde_json::Value = serde_json::from_slice(&STANDARD.decode(prop)?)?;
        data.pointer("/textures/SKIN/url")
            .and_then(|v| v.as_str())
            .context("此角色没有皮肤材质")?
            .to_owned()
    };
    let mut url = Url::parse(&url)?;
    if url.host_str() == Some("textures.minecraft.net") {
        let _ = url.set_scheme("https");
    }
    ensure!(
        url.scheme() == "https" && url.username().is_empty() && url.password().is_none(),
        "皮肤地址无效"
    );
    if p.kind == Kind::Microsoft {
        ensure!(
            url.host_str() == Some("textures.minecraft.net"),
            "皮肤来源无效"
        );
    }
    let mut data = Vec::new();
    http.get(url)
        .send()?
        .error_for_status()?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut data)?;
    ensure!(data.len() <= 1024 * 1024, "皮肤文件过大");
    atomic_write(&path, &portrait(&data)?)?;
    Ok(Some(path))
}
