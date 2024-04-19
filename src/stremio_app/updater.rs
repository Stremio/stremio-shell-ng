use std::{
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{anyhow, Context};
use semver::{Version, VersionReq};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

#[derive(Debug, Clone)]
pub struct Update {
    /// The new version that we update to
    pub version: Version,
    pub file: PathBuf,
}

#[derive(Debug)]
pub struct Updater {
    pub current_version: Version,
    pub next_version: VersionReq,
    pub endpoint: Url,
    pub force_update: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateResponse {
    version_desc: Url,
    version: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileItem {
    // name: String,
    pub url: Url,
    pub checksum: String,
    os: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    version: String,
    files: Vec<FileItem>,
}

impl Updater {
    pub fn new(current_version: Version, updater_endpoint: &Url, force_update: bool) -> Self {
        Self {
            next_version: VersionReq::parse(&format!(">{current_version}"))
                .expect("Version is type-safe"),
            current_version,
            endpoint: updater_endpoint.clone(),
            force_update,
        }
    }

    /// Fetches the latest update from the update server.
    pub fn autoupdate(&self) -> Result<Option<Update>, anyhow::Error> {
        // Check for updates
        println!("Fetching updates for v{}", self.current_version);
        println!("Using updater endpoint {}", &self.endpoint);
        let update_response =
            reqwest::blocking::get(self.endpoint.clone())?.json::<UpdateResponse>()?;
        let update_descriptor =
            reqwest::blocking::get(update_response.version_desc)?.json::<Descriptor>()?;

        if update_response.version != update_descriptor.version {
            return Err(anyhow!("Mismatched update versions"));
        }
        let installer = update_descriptor
            .files
            .iter()
            .find(|file_item| file_item.os == std::env::consts::OS)
            .context("No update for this OS")?;
        let version = Version::parse(update_descriptor.version.as_str())?;
        if !self.force_update && !self.next_version.matches(&version) {
            return Err(anyhow!(
                "No new releases found that match the requirement of `{}`",
                self.next_version
            ));
        }
        println!("Found update v{}", version);

        let file_name = std::path::Path::new(installer.url.path())
            .file_name()
            .context("Invalid file name")?
            .to_str()
            .context("The path is not valid UTF-8")?
            .to_string();
        let temp_dir = std::env::temp_dir();
        let dest = temp_dir.join(file_name);

        std::thread::sleep(std::time::Duration::from_secs(2));
        // Download the new setup file
        let mut installer_response = reqwest::blocking::get(installer.url.clone())?;
        let size = installer_response.content_length();
        let mut downloaded: u64 = 0;
        let mut sha256 = Sha256::new();

        println!("Downloading {} to {}", installer.url, dest.display());

        let mut chunk = [0u8; 8192];
        let mut file = std::fs::File::create(&dest)?;
        loop {
            let chunk_size = installer_response.read(&mut chunk)?;
            if chunk_size == 0 {
                break;
            }
            sha256.update(&chunk[..chunk_size]);
            file.write_all(&chunk[..chunk_size])?;
            if let Some(size) = size {
                downloaded += chunk_size as u64;
                print!("\rProgress: {}%", downloaded * 100 / size);
            } else {
                print!(".");
            }
            std::io::stdout().flush().ok();
        }
        println!();
        let actual_sha256 = format!("{:x}", sha256.finalize());
        if actual_sha256 != installer.checksum {
            std::fs::remove_file(dest)?;
            return Err(anyhow::anyhow!("Checksum verification failed"));
        }
        println!("Checksum verified.");

        let update = Some(Update {
            version,
            file: dest,
        });
        Ok(update)
    }
}
