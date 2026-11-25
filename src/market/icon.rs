use anyhow::{Result, ensure};
use image::{ImageFormat, ImageReader};
use sha2::{Digest, Sha256};
use std::{
    io::{Cursor, Read},
    path::{Path, PathBuf},
    time::Duration,
};
use url::Url;

const MAX_BYTES: u64 = 4 * 1024 * 1024;
const THUMBNAIL: u32 = 128;

fn thumbnail(bytes: Vec<u8>) -> Result<Vec<u8>> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    ensure!(
        matches!(
            reader.format(),
            Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Gif)
        ),
        "不支持的项目图标格式"
    );
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?;
    let mut output = Cursor::new(Vec::new());
    image
        .thumbnail(THUMBNAIL, THUMBNAIL)
        .write_to(&mut output, ImageFormat::Png)?;
    Ok(output.into_inner())
}

fn is_image_cdn(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|h| {
            matches!(h, "cdn.modrinth.com" | "cdn-raw.modrinth.com") || h.ends_with(".forgecdn.net")
        })
}

pub fn cached(root: &Path, address: &str) -> Result<PathBuf> {
    let url = Url::parse(address)?;
    ensure!(is_image_cdn(&url), "资源图标地址无效");
    let key = hex::encode(Sha256::digest(url.as_str().as_bytes()));
    let path = root
        .join("cache")
        .join("project-icons")
        .join(format!("{key}.png"));
    if path.is_file() {
        return Ok(path);
    }
    let response = crate::download::client_builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() <= 3 && is_image_cdn(attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()?
        .get(url)
        .send()?
        .error_for_status()?;
    ensure!(
        response.status().is_success() && response.content_length().is_none_or(|n| n <= MAX_BYTES),
        "资源图标不可用"
    );
    let mut bytes = Vec::new();
    response.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_BYTES, "资源图标过大");
    crate::library::atomic_write(&path, &thumbnail(bytes)?)?;
    Ok(path)
}
