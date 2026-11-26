use super::*;
use mc_launcher_core::core::version::{LibraryArtifact, LibraryDownloads};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::io::Read;

#[derive(Deserialize)]
struct Installer {
    install: Option<LegacyInstall>,
    #[serde(rename = "versionInfo")]
    version_info: Option<VersionJson>,
}

#[derive(Deserialize)]
struct LegacyInstall {
    minecraft: String,
    path: String,
    #[serde(rename = "filePath")]
    file_path: String,
}

// The original 1.12.2 installer bundles its universal JAR and a complete child profile.
pub(super) fn extract(
    jar: &Path,
    shared: &Path,
    mc: &str,
    build: &str,
    id: &str,
) -> Result<Option<VersionJson>> {
    let mut zip = zip::ZipArchive::new(File::open(jar)?)?;
    let metadata: Installer =
        serde_json::from_reader(zip.by_name("install_profile.json")?.take(4 * 1024 * 1024))?;
    let Some(install) = metadata.install else {
        return Ok(None);
    };
    let mut profile = metadata
        .version_info
        .context("旧版 Forge 安装包缺少版本描述")?;
    let coordinate = format!("net.minecraftforge:forge:{build}");
    let universal = format!("forge-{build}-universal.jar");
    ensure!(
        install.minecraft == mc && install.path == coordinate && install.file_path == universal,
        "旧版 Forge 安装包与所选版本不匹配"
    );
    let mut bytes = Vec::new();
    let entry = zip.by_name(&universal)?;
    ensure!(entry.size() <= 64 * 1024 * 1024, "Forge 运行库大小异常");
    entry.take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() <= 64 * 1024 * 1024,
        "Forge 运行库大小异常"
    );
    let relative = format!("net/minecraftforge/forge/{build}/forge-{build}.jar");
    let lib = profile
        .libraries
        .iter_mut()
        .find(|l| l.name == coordinate)
        .context("Forge 描述缺少主运行库")?;
    lib.downloads = Some(LibraryDownloads {
        artifact: Some(LibraryArtifact {
            path: relative.clone(),
            url: format!(
                "https://maven.minecraftforge.net/net/minecraftforge/forge/{build}/{universal}"
            ),
            sha1: hex::encode(Sha1::digest(&bytes)),
            size: bytes.len() as i64,
        }),
        classifiers: Default::default(),
    });
    for library in &mut profile.libraries {
        if library.downloads.is_none() && library.url.is_none() {
            library.url = Some("https://libraries.minecraft.net/".into());
        }
    }
    profile.id = Some(id.into());
    profile.inherits_from = Some(mc.into());
    atomic_write(&shared.join("libraries").join(relative), &bytes)?;
    atomic_write(
        &shared.join("versions").join(id).join(format!("{id}.json")),
        &serde_json::to_vec(&profile)?,
    )?;
    Ok(Some(profile))
}
