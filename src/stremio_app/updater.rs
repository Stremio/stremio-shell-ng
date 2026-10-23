use std::{
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{anyhow, Context};
use semver::{Version, VersionReq};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use super::constants::DEFAULT_AUTOUPDATER_ENDPOINT;

/// Endpoint the updater polls: the explicit one if given, else the default Laststream feed,
/// and `None` (no thread, no network call) when the updater is disabled.
pub fn resolve_endpoint(explicit: Option<Url>, disabled: bool) -> Option<Url> {
    if disabled {
        return None;
    }
    Some(explicit.unwrap_or_else(|| {
        Url::parse(DEFAULT_AUTOUPDATER_ENDPOINT).expect("Default updater endpoint is valid")
    }))
}

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
    /// Where the installer is downloaded.
    pub download_dir: PathBuf,
    /// Pause before the download starts.
    pub settle_delay: std::time::Duration,
    /// Version whose verified installer is already waiting to be run: not downloaded again.
    pub ready_version: Option<Version>,
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
            download_dir: std::env::temp_dir(),
            settle_delay: std::time::Duration::from_secs(2),
            ready_version: None,
        }
    }

    /// Fetches the latest update from the update server.
    pub fn autoupdate(&self) -> Result<Option<Update>, anyhow::Error> {
        // Check for updates
        println!("Fetching updates for v{}", self.current_version);
        println!("Using updater endpoint {}", self.endpoint);
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
        println!("Found update v{version}");

        let file_name = std::path::Path::new(installer.url.path())
            .file_name()
            .context("Invalid file name")?
            .to_str()
            .context("The path is not valid UTF-8")?
            .to_string();
        let dest = self.download_dir.join(file_name);
        if self.ready_version.as_ref() == Some(&version) && dest.is_file() {
            println!("Update v{version} is already downloaded and verified");
            return Ok(None);
        }
        // Downloaded aside and renamed once verified, so `dest` is never a partial or unverified file
        // (a previously verified installer may still be waiting to be run from it).
        let partial = dest.with_extension("exe.part");

        std::thread::sleep(self.settle_delay);
        // Download the new setup file
        let mut installer_response = reqwest::blocking::get(installer.url.clone())?;
        let size = installer_response.content_length();
        let mut downloaded: u64 = 0;
        let mut sha256 = Sha256::new();

        println!("Downloading {} to {}", installer.url, partial.display());

        let mut chunk = [0u8; 8192];
        let mut file = std::fs::File::create(&partial)?;
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
        if !actual_sha256.eq_ignore_ascii_case(installer.checksum.trim()) {
            std::fs::remove_file(&partial)?;
            return Err(anyhow::anyhow!("Checksum verification failed"));
        }
        drop(file);
        std::fs::rename(&partial, &dest)?;
        println!("Checksum verified.");

        let update = Some(Update {
            version,
            file: dest,
        });
        Ok(update)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    const INSTALLER: &[u8] = b"fake installer bytes";

    fn sha256_hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    /// Serves `latest.json` (feed and descriptor at once, like `upload.ps1` publishes it)
    /// and the installer on 127.0.0.1; counts the requests received.
    fn serve(feed_version: &str, checksum: &str) -> (Url, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let feed = format!(
            r#"{{"version":"{feed_version}","versionDesc":"{base}/latest.json","files":[{{"name":"Laststream.exe","url":"{base}/Laststream.exe","checksum":"{checksum}","os":"windows"}}]}}"#
        );
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_server = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                hits_server.fetch_add(1, Ordering::SeqCst);
                let mut request_line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut request_line)
                    .ok();
                let (status, content_type, body) = if request_line.contains("/latest.json") {
                    ("200 OK", "application/json", feed.clone().into_bytes())
                } else if request_line.contains("/Laststream.exe") {
                    ("200 OK", "application/octet-stream", INSTALLER.to_vec())
                } else {
                    ("404 Not Found", "text/plain", Vec::new())
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).ok();
                stream.write_all(&body).ok();
            }
        });
        (Url::parse(&format!("{base}/latest.json")).unwrap(), hits)
    }

    fn updater_for(endpoint: &Url, current: &str, name: &str) -> (Updater, PathBuf) {
        let dir = std::env::temp_dir().join(format!("laststream-updater-test-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let mut updater = Updater::new(current.parse().unwrap(), endpoint, false);
        updater.download_dir = dir.clone();
        updater.settle_delay = Duration::ZERO;
        (updater, dir)
    }

    #[test]
    fn default_endpoint_is_the_laststream_feed() {
        let endpoint = resolve_endpoint(None, false).expect("enabled by default");
        assert_eq!(endpoint.as_str(), DEFAULT_AUTOUPDATER_ENDPOINT);
        assert!(endpoint.path().ends_with("/app/windows/latest.json"));
    }

    #[test]
    fn explicit_endpoint_overrides_the_default() {
        let custom = Url::parse("https://example.test/feed.json").unwrap();
        assert_eq!(resolve_endpoint(Some(custom.clone()), false), Some(custom));
    }

    #[test]
    fn disabled_updater_has_no_endpoint_even_when_one_is_given() {
        assert_eq!(resolve_endpoint(None, true), None);
        let custom = Url::parse("https://example.test/feed.json").unwrap();
        assert_eq!(resolve_endpoint(Some(custom), true), None);
    }

    #[test]
    fn newer_version_is_downloaded_and_checksum_verified() {
        let (endpoint, hits) = serve("1.0.18", &sha256_hex(INSTALLER));
        let (updater, dir) = updater_for(&endpoint, "1.0.17", "ok");
        let update = updater.autoupdate().unwrap().expect("update expected");
        assert_eq!(update.version, Version::new(1, 0, 18));
        assert_eq!(std::fs::read(&update.file).unwrap(), INSTALLER);
        assert_eq!(update.file, dir.join("Laststream.exe"));
        assert_eq!(
            hits.load(Ordering::SeqCst),
            3,
            "feed, descriptor, installer"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn uppercase_checksum_in_the_feed_is_accepted() {
        let (endpoint, _) = serve("1.0.18", &sha256_hex(INSTALLER).to_uppercase());
        let (updater, dir) = updater_for(&endpoint, "1.0.17", "upper");
        assert!(updater.autoupdate().unwrap().is_some());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn checksum_mismatch_rejects_and_deletes_the_installer() {
        let (endpoint, _) = serve("1.0.18", &sha256_hex(b"something else"));
        let (updater, dir) = updater_for(&endpoint, "1.0.17", "bad");
        let error = updater.autoupdate().unwrap_err();
        assert!(error.to_string().contains("Checksum"), "{error}");
        assert!(!dir.join("Laststream.exe").exists());
        assert!(!dir.join("Laststream.exe.part").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn failed_check_keeps_the_installer_already_waiting() {
        let (endpoint, _) = serve("1.0.19", &sha256_hex(b"something else"));
        let (updater, dir) = updater_for(&endpoint, "1.0.17", "keep");
        std::fs::write(dir.join("Laststream.exe"), b"verified earlier").unwrap();
        assert!(updater.autoupdate().is_err());
        assert_eq!(
            std::fs::read(dir.join("Laststream.exe")).unwrap(),
            b"verified earlier"
        );
        assert!(!dir.join("Laststream.exe.part").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn version_already_downloaded_is_not_fetched_again() {
        let (endpoint, hits) = serve("1.0.18", &sha256_hex(INSTALLER));
        let (mut updater, dir) = updater_for(&endpoint, "1.0.17", "ready");
        std::fs::write(dir.join("Laststream.exe"), INSTALLER).unwrap();
        updater.ready_version = Some(Version::new(1, 0, 18));
        assert!(updater.autoupdate().unwrap().is_none());
        assert_eq!(hits.load(Ordering::SeqCst), 2, "installer not requested");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn same_or_older_version_is_not_downloaded() {
        for (published, name) in [("1.0.17", "same"), ("1.0.16", "older")] {
            let (endpoint, hits) = serve(published, &sha256_hex(INSTALLER));
            let (updater, dir) = updater_for(&endpoint, "1.0.17", name);
            assert!(updater.autoupdate().is_err());
            assert_eq!(hits.load(Ordering::SeqCst), 2, "installer not requested");
            assert!(!dir.join("Laststream.exe").exists());
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn force_update_reinstalls_the_current_version() {
        let (endpoint, _) = serve("1.0.17", &sha256_hex(INSTALLER));
        let (mut updater, dir) = updater_for(&endpoint, "1.0.17", "force");
        updater.force_update = true;
        assert!(updater.autoupdate().unwrap().is_some());
        std::fs::remove_dir_all(dir).ok();
    }
}
