use std::{
    io::{Read, Write},
    path::PathBuf,
    time::Duration,
};

use anyhow::{anyhow, Context};
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use semver::{Version, VersionReq};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use super::constants::{
    AUTOUPDATER_ALLOWED_HOSTS, AUTOUPDATER_PUBLIC_KEY_HEX, DEFAULT_AUTOUPDATER_ENDPOINT,
};

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

/// File name of the downloaded installer, fixed so a stale one is overwritten, never accumulated.
const INSTALLER_FILE_NAME: &str = "Laststream-update.exe";

/// Time allowed to open a connection, for every request.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Time allowed for a whole feed / descriptor / signature request (they are a few KiB).
const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
/// Time allowed for the whole installer download (a stalled read ends the request at this deadline).
const INSTALLER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// A feed, descriptor or signature larger than this is refused.
const METADATA_MAX_BYTES: u64 = 1024 * 1024;
/// Redirect hops followed at most; each one is checked like the original URL.
const MAX_REDIRECTS: usize = 3;

/// Only `https` is accepted: the SHA-256 comes from the descriptor, so a plain-http descriptor
/// would let a network attacker serve both the installer and its hash. Tests may use 127.0.0.1.
fn require_https(url: &Url, what: &str) -> Result<(), anyhow::Error> {
    if url.scheme() == "https" {
        return Ok(());
    }
    #[cfg(test)]
    if url.scheme() == "http" && url.host_str() == Some("127.0.0.1") {
        return Ok(());
    }
    Err(anyhow!("Refusing non-HTTPS {what} URL: {url}"))
}

/// A URL the updater may fetch: HTTPS and a host from the allow-list (exact, case-insensitive).
fn check_url(url: &Url, what: &str, allowed_hosts: &[String]) -> Result<(), anyhow::Error> {
    require_https(url, what)?;
    let host = url.host_str().unwrap_or_default();
    if allowed_hosts.iter().any(|h| h.eq_ignore_ascii_case(host)) {
        return Ok(());
    }
    Err(anyhow!(
        "Refusing {what} URL on a host that is not allowed: {url}"
    ))
}

/// Client whose redirects (3 at most) are followed only while every hop passes `check_url`,
/// so a redirect can neither downgrade to http nor leave the allowed hosts. Connect timeout set;
/// each request sets its own total timeout.
fn updater_client(allowed_hosts: &[String]) -> Result<reqwest::blocking::Client, anyhow::Error> {
    let allowed_hosts = allowed_hosts.to_vec();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("Too many redirects");
        }
        match check_url(attempt.url(), "redirect", &allowed_hosts) {
            Ok(()) => attempt.follow(),
            Err(error) => attempt.error(error.to_string()),
        }
    });
    Ok(reqwest::blocking::Client::builder()
        .redirect(policy)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(None)
        .build()?)
}

/// Parses the embedded (or injected) ed25519 public key: 32 bytes as 64 hex characters.
fn parse_public_key(hex: &str) -> Result<VerifyingKey, anyhow::Error> {
    let hex = hex.trim();
    if hex.len() != 64 || !hex.is_ascii() {
        return Err(anyhow!("Malformed update public key"));
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| anyhow!("Malformed update public key"))?;
    }
    VerifyingKey::from_bytes(&bytes).map_err(|_| anyhow!("Malformed update public key"))
}

/// `<url>.sig`: same host and path with `.sig` appended (query and fragment dropped).
fn signature_url(url: &Url) -> Url {
    let mut sig = url.clone();
    sig.set_query(None);
    sig.set_fragment(None);
    sig.set_path(&format!("{}.sig", url.path()));
    sig
}

fn fetch_limited(
    client: &reqwest::blocking::Client,
    url: &Url,
    what: &str,
) -> Result<Vec<u8>, anyhow::Error> {
    let response = client
        .get(url.clone())
        .timeout(METADATA_TIMEOUT)
        .send()
        .with_context(|| format!("Could not fetch the {what}"))?
        .error_for_status()
        .with_context(|| format!("Could not fetch the {what}"))?;
    let mut body = Vec::new();
    response
        .take(METADATA_MAX_BYTES + 1)
        .read_to_end(&mut body)
        .with_context(|| format!("Could not read the {what}"))?;
    if body.len() as u64 > METADATA_MAX_BYTES {
        return Err(anyhow!("The {what} is too large"));
    }
    Ok(body)
}

/// Fetches `url` and its detached `.sig`, and returns the exact bytes only if the signature
/// (base64 ed25519 over those bytes) is valid for `key`. Nothing is parsed before this succeeds.
fn fetch_signed(
    client: &reqwest::blocking::Client,
    url: &Url,
    what: &str,
    allowed_hosts: &[String],
    key: &VerifyingKey,
) -> Result<Vec<u8>, anyhow::Error> {
    check_url(url, what, allowed_hosts)?;
    let sig_url = signature_url(url);
    check_url(&sig_url, "signature", allowed_hosts)?;
    let body = fetch_limited(client, url, what)?;
    let sig_text = fetch_limited(client, &sig_url, "signature")?;
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(
            String::from_utf8_lossy(&sig_text)
                .split_whitespace()
                .collect::<String>(),
        )
        .map_err(|_| anyhow!("The {what} signature is not valid base64"))?;
    let signature = Signature::from_slice(&sig_bytes)
        .map_err(|_| anyhow!("The {what} signature has the wrong length"))?;
    key.verify_strict(&body, &signature)
        .map_err(|_| anyhow!("The {what} signature is invalid: update refused"))?;
    Ok(body)
}

fn file_sha256(path: &std::path::Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut sha256 = Sha256::new();
    let mut chunk = [0u8; 8192];
    loop {
        let size = file.read(&mut chunk)?;
        if size == 0 {
            break;
        }
        sha256.update(&chunk[..size]);
    }
    Ok(format!("{:x}", sha256.finalize()))
}

#[derive(Debug, Clone)]
pub struct Update {
    /// The new version that we update to
    pub version: Version,
    pub file: PathBuf,
    /// SHA-256 (hex) the installer must still have when it is run, from the descriptor.
    pub sha256: String,
}

impl Update {
    /// Re-hashes the installer right before it is run: the file sits in a user-writable
    /// temp folder, possibly for hours after the download check (TOCTOU). On mismatch (or if it
    /// is no longer a regular file) the file is deleted and an error is returned, never the path.
    pub fn verify_before_run(&self) -> Result<&std::path::Path, anyhow::Error> {
        let is_regular = std::fs::symlink_metadata(&self.file)
            .map(|meta| meta.is_file())
            .unwrap_or(false);
        let matches = is_regular
            && file_sha256(&self.file)
                .map(|hash| hash.eq_ignore_ascii_case(self.sha256.trim()))
                .unwrap_or(false);
        if matches {
            return Ok(&self.file);
        }
        if let Err(error) = std::fs::remove_file(&self.file) {
            println!("Could not remove {}: {error}", self.file.display());
        }
        Err(anyhow!(
            "The update installer changed after it was verified; it was refused and deleted"
        ))
    }
}

/// Takes the update waiting in `slot` and returns the installer path to run, only if it still
/// passes `verify_before_run`. On refusal the slot is emptied and `None` is returned: the caller
/// just skips the launch (it must not leave its own loop).
pub fn verified_installer_to_run(slot: &std::sync::Mutex<Option<Update>>) -> Option<PathBuf> {
    let update = slot.lock().unwrap().clone()?;
    match update.verify_before_run() {
        Ok(path) => Some(path.to_path_buf()),
        Err(error) => {
            eprintln!("Updater refused: {error}");
            *slot.lock().unwrap() = None;
            None
        }
    }
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
    /// ed25519 public key (hex) the feed and descriptor signatures are checked with.
    /// The real key by default; only tests replace it.
    pub public_key_hex: String,
    /// Hosts the feed, descriptor, installer and redirects may point to.
    pub allowed_hosts: Vec<String>,
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
            download_dir: std::env::temp_dir().join("laststream-update"),
            settle_delay: std::time::Duration::from_secs(2),
            ready_version: None,
            public_key_hex: AUTOUPDATER_PUBLIC_KEY_HEX.to_string(),
            allowed_hosts: AUTOUPDATER_ALLOWED_HOSTS
                .iter()
                .map(|host| host.to_string())
                .collect(),
        }
    }

    /// Fetches the latest update from the update server. The feed and the descriptor are only
    /// trusted (parsed) once their ed25519 signature checks out, so the installer SHA-256 they
    /// carry is authenticated. Any failure leaves no installer and no `.part` behind.
    pub fn autoupdate(&self) -> Result<Option<Update>, anyhow::Error> {
        // Check for updates
        println!("Fetching updates for v{}", self.current_version);
        println!("Using updater endpoint {}", self.endpoint);
        let result = self.check_signed_update();
        if let Err(error) = &result {
            eprintln!("Updater: update refused or failed: {error:#}");
        }
        result
    }

    fn check_signed_update(&self) -> Result<Option<Update>, anyhow::Error> {
        let key = parse_public_key(&self.public_key_hex)?;
        check_url(&self.endpoint, "updater endpoint", &self.allowed_hosts)?;
        let client = updater_client(&self.allowed_hosts)?;
        let feed_bytes = fetch_signed(
            &client,
            &self.endpoint,
            "update feed",
            &self.allowed_hosts,
            &key,
        )?;
        let update_response: UpdateResponse =
            serde_json::from_slice(&feed_bytes).context("Invalid update feed")?;
        check_url(
            &update_response.version_desc,
            "update descriptor",
            &self.allowed_hosts,
        )?;
        // The feed and the descriptor are usually the same signed file: not fetched twice.
        let descriptor_bytes = if update_response.version_desc == self.endpoint {
            feed_bytes
        } else {
            fetch_signed(
                &client,
                &update_response.version_desc,
                "update descriptor",
                &self.allowed_hosts,
                &key,
            )?
        };
        let update_descriptor: Descriptor =
            serde_json::from_slice(&descriptor_bytes).context("Invalid update descriptor")?;

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

        check_url(&installer.url, "installer", &self.allowed_hosts)?;
        std::fs::create_dir_all(&self.download_dir)?;
        let dest = self.download_dir.join(INSTALLER_FILE_NAME);
        // An installer already on disk is reused only if its hash still matches the signed descriptor.
        if dest.is_file()
            && file_sha256(&dest)
                .map(|hash| hash.eq_ignore_ascii_case(installer.checksum.trim()))
                .unwrap_or(false)
        {
            println!("Update v{version} is already downloaded and verified");
            if self.ready_version.as_ref() == Some(&version) {
                return Ok(None);
            }
            return Ok(Some(Update {
                version,
                file: dest,
                sha256: installer.checksum.trim().to_string(),
            }));
        }
        // Downloaded aside and renamed once verified, so `dest` is never a partial or unverified file
        // (a previously verified installer may still be waiting to be run from it).
        let partial = dest.with_extension("exe.part");

        std::thread::sleep(self.settle_delay);
        if let Err(error) = download_verified(&client, installer, &partial, &dest) {
            if let Err(remove_error) = std::fs::remove_file(&partial) {
                if remove_error.kind() != std::io::ErrorKind::NotFound {
                    println!("Could not remove {}: {remove_error}", partial.display());
                }
            }
            return Err(error);
        }
        println!("Checksum verified.");

        Ok(Some(Update {
            version,
            file: dest,
            sha256: installer.checksum.trim().to_string(),
        }))
    }
}

/// Downloads the installer to `partial`, checks its SHA-256 against the (signed) descriptor and
/// renames it to `dest`. On `Err` the caller deletes `partial`.
fn download_verified(
    client: &reqwest::blocking::Client,
    installer: &FileItem,
    partial: &std::path::Path,
    dest: &std::path::Path,
) -> Result<(), anyhow::Error> {
    let mut installer_response = client
        .get(installer.url.clone())
        .timeout(INSTALLER_TIMEOUT)
        .send()?
        .error_for_status()?;
    let size = installer_response.content_length();
    let mut downloaded: u64 = 0;
    let mut sha256 = Sha256::new();

    println!("Downloading {} to {}", installer.url, partial.display());

    let mut chunk = [0u8; 8192];
    let mut file = std::fs::File::create(partial)?;
    loop {
        let chunk_size = installer_response.read(&mut chunk)?;
        if chunk_size == 0 {
            break;
        }
        sha256.update(&chunk[..chunk_size]);
        file.write_all(&chunk[..chunk_size])?;
        if let Some(size) = size.filter(|size| *size > 0) {
            downloaded += chunk_size as u64;
            print!("\rProgress: {}%", downloaded * 100 / size);
        } else {
            print!(".");
        }
        std::io::stdout().flush().ok();
    }
    println!();
    let actual_sha256 = format!("{:x}", sha256.finalize());
    drop(file);
    if !actual_sha256.eq_ignore_ascii_case(installer.checksum.trim()) {
        return Err(anyhow!("Checksum verification failed"));
    }
    std::fs::rename(partial, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    const INSTALLER: &[u8] = b"fake installer bytes";

    fn sha256_hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    /// Key pair generated on the fly (random seed from the OS), never stored anywhere.
    fn new_signing_key() -> SigningKey {
        let mut seed = [0u8; 32];
        seed[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        seed[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        SigningKey::from_bytes(&seed)
    }

    fn public_hex(key: &SigningKey) -> String {
        key.verifying_key()
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Sig {
        /// `.sig` made by the trusted key over the exact served bytes.
        Valid,
        /// No `.sig` published (404).
        Missing,
        /// `.sig` made by another key.
        OtherKey,
        /// `.sig` valid for the original file, but the served file has one more byte.
        Tampered,
        /// `.sig` that is not base64.
        NotBase64,
        /// `.sig` that is base64 but not 64 bytes.
        WrongLength,
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Body {
        Normal,
        /// Announces more bytes than it sends, then closes the connection.
        Truncated,
    }

    struct Setup {
        feed_version: &'static str,
        checksum: String,
        sig: Sig,
        body: Body,
        /// Descriptor is a different file (`/desc.json`, with its own `.sig`) than the feed.
        separate_descriptor: bool,
        /// Installer URL base written in the signed feed (default: the local server).
        installer_base: Option<&'static str>,
    }

    impl Setup {
        fn new(feed_version: &'static str, checksum: &str) -> Self {
            Setup {
                feed_version,
                checksum: checksum.to_string(),
                sig: Sig::Valid,
                body: Body::Normal,
                separate_descriptor: false,
                installer_base: None,
            }
        }
    }

    struct Served {
        endpoint: Url,
        hits: Arc<AtomicUsize>,
        public_key_hex: String,
    }

    fn serve(feed_version: &'static str, checksum: &str) -> Served {
        serve_with(Setup::new(feed_version, checksum))
    }

    /// Serves `latest.json` (feed and descriptor at once, like `upload.ps1` publishes it),
    /// its `.sig` and the installer on 127.0.0.1; counts the requests received.
    fn serve_with(setup: Setup) -> Served {
        let trusted = new_signing_key();
        let other = new_signing_key();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let installer_base = setup.installer_base.map_or(base.clone(), str::to_string);
        let desc_path = if setup.separate_descriptor {
            "desc.json"
        } else {
            "latest.json"
        };
        let files = format!(
            r#""files":[{{"name":"Laststream.exe","url":"{installer_base}/Laststream.exe","checksum":"{}","os":"windows"}}]"#,
            setup.checksum
        );
        let feed = format!(
            r#"{{"version":"{}","versionDesc":"{base}/{desc_path}",{files}}}"#,
            setup.feed_version
        )
        .into_bytes();
        let descriptor = format!(r#"{{"version":"{}",{files}}}"#, setup.feed_version).into_bytes();
        let signer = if setup.sig == Sig::OtherKey {
            other
        } else {
            trusted.clone()
        };
        let sign = |data: &[u8]| {
            base64::engine::general_purpose::STANDARD
                .encode(signer.sign(data).to_bytes())
                .into_bytes()
        };
        let (feed_sig, descriptor_sig) = (sign(&feed), sign(&descriptor));
        let mut served_feed = feed.clone();
        if setup.sig == Sig::Tampered {
            served_feed.push(b' ');
        }
        let (sig_mode, body_mode) = (setup.sig, setup.body);
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
                if request_line.contains("/redirect.json") {
                    stream
                        .write_all(b"HTTP/1.1 302 Found\r\nLocation: http://example.invalid/latest.json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .ok();
                    continue;
                }
                if request_line.contains("/redirect-https.json") {
                    stream
                        .write_all(b"HTTP/1.1 302 Found\r\nLocation: https://evil.invalid/latest.json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .ok();
                    continue;
                }
                if request_line.contains("/Laststream.exe") && body_mode == Body::Truncated {
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n0123456789")
                        .ok();
                    continue;
                }
                let (status, content_type, body) = if request_line.contains("/latest.json.sig") {
                    match sig_mode {
                        Sig::Missing => ("404 Not Found", "text/plain", Vec::new()),
                        Sig::NotBase64 => ("200 OK", "text/plain", b"!!!not base64!!!".to_vec()),
                        Sig::WrongLength => ("200 OK", "text/plain", b"AAAA".to_vec()),
                        _ => ("200 OK", "text/plain", feed_sig.clone()),
                    }
                } else if request_line.contains("/desc.json.sig") {
                    if sig_mode == Sig::Missing {
                        ("404 Not Found", "text/plain", Vec::new())
                    } else {
                        ("200 OK", "text/plain", descriptor_sig.clone())
                    }
                } else if request_line.contains("/latest.json") {
                    ("200 OK", "application/json", served_feed.clone())
                } else if request_line.contains("/desc.json") {
                    ("200 OK", "application/json", descriptor.clone())
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
        Served {
            endpoint: Url::parse(&format!("{base}/latest.json")).unwrap(),
            hits,
            public_key_hex: public_hex(&trusted),
        }
    }

    /// Test folders live under the worktree (`target/test-tmp`), not in the system temp folder.
    fn updater_for(served: &Served, current: &str, name: &str) -> (Updater, PathBuf) {
        updater_at(&served.endpoint, &served.public_key_hex, current, name)
    }

    fn updater_at(endpoint: &Url, key_hex: &str, current: &str, name: &str) -> (Updater, PathBuf) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!("laststream-updater-test-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let mut updater = Updater::new(current.parse().unwrap(), endpoint, false);
        updater.download_dir = dir.clone();
        updater.settle_delay = Duration::ZERO;
        updater.public_key_hex = key_hex.to_string();
        updater.allowed_hosts = vec!["127.0.0.1".to_string()];
        (updater, dir)
    }

    fn assert_nothing_left(dir: &std::path::Path) {
        assert!(!dir.join(INSTALLER_FILE_NAME).exists(), "no installer");
        assert!(
            !dir.join("Laststream-update.exe.part").exists(),
            "no .part file"
        );
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
    fn real_public_key_and_host_are_the_defaults() {
        let endpoint = Url::parse(DEFAULT_AUTOUPDATER_ENDPOINT).unwrap();
        let updater = Updater::new(Version::new(1, 0, 0), &endpoint, false);
        assert_eq!(updater.public_key_hex, AUTOUPDATER_PUBLIC_KEY_HEX);
        assert!(parse_public_key(&updater.public_key_hex).is_ok());
        assert_eq!(
            updater.allowed_hosts,
            vec!["laststream.gtux-prog.workers.dev"]
        );
        assert!(check_url(&endpoint, "t", &updater.allowed_hosts).is_ok());
    }

    #[test]
    fn newer_version_is_downloaded_and_checksum_verified() {
        let served = serve("1.0.18", &sha256_hex(INSTALLER));
        let (updater, dir) = updater_for(&served, "1.0.17", "ok");
        let update = updater.autoupdate().unwrap().expect("update expected");
        assert_eq!(update.version, Version::new(1, 0, 18));
        assert_eq!(std::fs::read(&update.file).unwrap(), INSTALLER);
        assert_eq!(update.file, dir.join(INSTALLER_FILE_NAME));
        assert_eq!(
            served.hits.load(Ordering::SeqCst),
            3,
            "feed, its signature, installer"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn separate_signed_descriptor_is_fetched_and_verified() {
        let mut setup = Setup::new("1.0.18", &sha256_hex(INSTALLER));
        setup.separate_descriptor = true;
        let served = serve_with(setup);
        let (updater, dir) = updater_for(&served, "1.0.17", "separate");
        assert!(updater.autoupdate().unwrap().is_some());
        assert_eq!(
            served.hits.load(Ordering::SeqCst),
            5,
            "feed, sig, descriptor, sig, installer"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn separate_descriptor_without_signature_is_refused() {
        let mut setup = Setup::new("1.0.18", &sha256_hex(INSTALLER));
        setup.separate_descriptor = true;
        setup.sig = Sig::Missing;
        let served = serve_with(setup);
        let (updater, dir) = updater_for(&served, "1.0.17", "separate-nosig");
        assert!(updater.autoupdate().is_err());
        assert_nothing_left(&dir);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn uppercase_checksum_in_the_feed_is_accepted() {
        let served = serve("1.0.18", &sha256_hex(INSTALLER).to_uppercase());
        let (updater, dir) = updater_for(&served, "1.0.17", "upper");
        assert!(updater.autoupdate().unwrap().is_some());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn checksum_mismatch_rejects_and_deletes_the_installer() {
        let served = serve("1.0.18", &sha256_hex(b"something else"));
        let (updater, dir) = updater_for(&served, "1.0.17", "bad");
        let error = updater.autoupdate().unwrap_err();
        assert!(error.to_string().contains("Checksum"), "{}", error);
        assert_nothing_left(&dir);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn wrong_sha_installer_is_never_run() {
        let served = serve("1.0.18", &sha256_hex(b"something else"));
        let (updater, dir) = updater_for(&served, "1.0.17", "never-run");
        let slot = std::sync::Mutex::new(None);
        match updater.autoupdate() {
            Ok(update) => *slot.lock().unwrap() = update,
            Err(error) => assert!(error.to_string().contains("Checksum"), "{}", error),
        }
        assert!(slot.lock().unwrap().is_none(), "no Update was produced");
        assert_eq!(verified_installer_to_run(&slot), None);
        assert_nothing_left(&dir);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn interrupted_download_removes_the_part_file() {
        let mut setup = Setup::new("1.0.18", &sha256_hex(INSTALLER));
        setup.body = Body::Truncated;
        let served = serve_with(setup);
        let (updater, dir) = updater_for(&served, "1.0.17", "truncated");
        // A stale .part from an earlier crash must not survive either.
        std::fs::write(dir.join("Laststream-update.exe.part"), b"stale").unwrap();
        assert!(updater.autoupdate().is_err());
        assert_nothing_left(&dir);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn failed_check_keeps_the_installer_already_waiting() {
        let served = serve("1.0.19", &sha256_hex(b"something else"));
        let (updater, dir) = updater_for(&served, "1.0.17", "keep");
        std::fs::write(dir.join(INSTALLER_FILE_NAME), b"verified earlier").unwrap();
        assert!(updater.autoupdate().is_err());
        assert_eq!(
            std::fs::read(dir.join(INSTALLER_FILE_NAME)).unwrap(),
            b"verified earlier"
        );
        assert!(!dir.join("Laststream-update.exe.part").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn version_already_downloaded_is_not_fetched_again() {
        let served = serve("1.0.18", &sha256_hex(INSTALLER));
        let (mut updater, dir) = updater_for(&served, "1.0.17", "ready");
        std::fs::write(dir.join(INSTALLER_FILE_NAME), INSTALLER).unwrap();
        updater.ready_version = Some(Version::new(1, 0, 18));
        assert!(updater.autoupdate().unwrap().is_none());
        assert_eq!(
            served.hits.load(Ordering::SeqCst),
            2,
            "installer not requested"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn same_or_older_version_is_not_downloaded() {
        for (published, name) in [("1.0.17", "same"), ("1.0.16", "older")] {
            let served = serve(published, &sha256_hex(INSTALLER));
            let (updater, dir) = updater_for(&served, "1.0.17", name);
            assert!(updater.autoupdate().is_err());
            assert_eq!(
                served.hits.load(Ordering::SeqCst),
                2,
                "installer not requested"
            );
            assert!(!dir.join(INSTALLER_FILE_NAME).exists());
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn plain_http_urls_are_rejected() {
        let local = Url::parse("http://127.0.0.1:1/x").unwrap();
        assert!(
            require_https(&local, "t").is_ok(),
            "loopback allowed in tests"
        );
        for bad in [
            "http://example.com/x",
            "http://localhost/x",
            "ftp://example.com/x",
        ] {
            let url = Url::parse(bad).unwrap();
            let error = require_https(&url, "installer").unwrap_err();
            assert!(error.to_string().contains("non-HTTPS"), "{}", error);
        }
        assert!(require_https(&Url::parse("https://example.com/x").unwrap(), "t").is_ok());
    }

    #[test]
    fn non_https_endpoint_is_refused_without_any_request() {
        let endpoint = Url::parse("http://example.invalid/latest.json").unwrap();
        let (updater, dir) =
            updater_at(&endpoint, &public_hex(&new_signing_key()), "1.0.17", "http");
        let error = updater.autoupdate().unwrap_err();
        assert!(error.to_string().contains("non-HTTPS"), "{}", error);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn valid_installer_already_on_disk_is_not_downloaded_again() {
        let served = serve("1.0.18", &sha256_hex(INSTALLER));
        let (updater, dir) = updater_for(&served, "1.0.17", "on-disk");
        std::fs::write(dir.join(INSTALLER_FILE_NAME), INSTALLER).unwrap();
        let update = updater.autoupdate().unwrap().expect("reused installer");
        assert_eq!(update.file, dir.join(INSTALLER_FILE_NAME));
        assert_eq!(
            served.hits.load(Ordering::SeqCst),
            2,
            "installer not requested"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn corrupt_installer_on_disk_is_downloaded_again() {
        let served = serve("1.0.18", &sha256_hex(INSTALLER));
        let (mut updater, dir) = updater_for(&served, "1.0.17", "corrupt");
        std::fs::write(dir.join(INSTALLER_FILE_NAME), b"tampered").unwrap();
        updater.ready_version = Some(Version::new(1, 0, 18));
        assert!(updater.autoupdate().unwrap().is_some());
        assert_eq!(
            std::fs::read(dir.join(INSTALLER_FILE_NAME)).unwrap(),
            INSTALLER
        );
        assert_eq!(served.hits.load(Ordering::SeqCst), 3);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn redirect_to_plain_http_is_refused() {
        let served = serve("1.0.18", &sha256_hex(INSTALLER));
        let endpoint = served.endpoint.join("/redirect.json").unwrap();
        let (updater, dir) = updater_at(&endpoint, &served.public_key_hex, "1.0.17", "redirect");
        let error = updater.autoupdate().unwrap_err();
        assert!(format!("{:?}", error).contains("non-HTTPS"), "{:?}", error);
        assert_eq!(
            served.hits.load(Ordering::SeqCst),
            1,
            "only the redirect was hit"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn redirect_to_a_host_outside_the_list_is_refused() {
        let served = serve("1.0.18", &sha256_hex(INSTALLER));
        let endpoint = served.endpoint.join("/redirect-https.json").unwrap();
        let (updater, dir) =
            updater_at(&endpoint, &served.public_key_hex, "1.0.17", "redirect-host");
        let error = updater.autoupdate().unwrap_err();
        assert!(
            format!("{:?}", error).contains("not allowed"),
            "{:?}",
            error
        );
        assert_eq!(served.hits.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn installer_replaced_after_download_is_refused_and_deleted() {
        let served = serve("1.0.18", &sha256_hex(INSTALLER));
        let (updater, dir) = updater_for(&served, "1.0.17", "toctou");
        let update = updater.autoupdate().unwrap().expect("update expected");
        assert_eq!(update.verify_before_run().unwrap(), update.file.as_path());
        std::fs::write(&update.file, b"malicious replacement").unwrap();
        let error = update.verify_before_run().unwrap_err();
        assert!(error.to_string().contains("changed"), "{}", error);
        assert!(!update.file.exists(), "tampered installer must be deleted");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn installer_removed_after_download_is_refused() {
        let served = serve("1.0.18", &sha256_hex(INSTALLER));
        let (updater, dir) = updater_for(&served, "1.0.17", "toctou-gone");
        let update = updater.autoupdate().unwrap().expect("update expected");
        std::fs::remove_file(&update.file).unwrap();
        assert!(update.verify_before_run().is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn refusal_empties_the_slot_and_returns_none() {
        let served = serve("1.0.18", &sha256_hex(INSTALLER));
        let (updater, dir) = updater_for(&served, "1.0.17", "slot");
        let update = updater.autoupdate().unwrap().expect("update expected");
        let slot = std::sync::Mutex::new(Some(update.clone()));
        assert_eq!(verified_installer_to_run(&slot), Some(update.file.clone()));
        assert!(slot.lock().unwrap().is_some());
        std::fs::write(&update.file, b"malicious replacement").unwrap();
        assert_eq!(verified_installer_to_run(&slot), None);
        assert!(slot.lock().unwrap().is_none());
        assert_eq!(verified_installer_to_run(&slot), None, "empty slot");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn directory_in_place_of_the_installer_is_refused() {
        let served = serve("1.0.18", &sha256_hex(INSTALLER));
        let (updater, dir) = updater_for(&served, "1.0.17", "dir-swap");
        let update = updater.autoupdate().unwrap().expect("update expected");
        std::fs::remove_file(&update.file).unwrap();
        std::fs::create_dir(&update.file).unwrap();
        assert!(update.verify_before_run().is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn uppercase_descriptor_checksum_still_verifies_before_run() {
        let served = serve("1.0.18", &sha256_hex(INSTALLER).to_uppercase());
        let (updater, dir) = updater_for(&served, "1.0.17", "upper-run");
        let update = updater.autoupdate().unwrap().expect("update expected");
        assert!(update.verify_before_run().is_ok());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn force_update_reinstalls_the_current_version() {
        let served = serve("1.0.17", &sha256_hex(INSTALLER));
        let (mut updater, dir) = updater_for(&served, "1.0.17", "force");
        updater.force_update = true;
        assert!(updater.autoupdate().unwrap().is_some());
        std::fs::remove_dir_all(dir).ok();
    }

    // --- LAST-88: signature, host list ---

    #[test]
    fn descriptor_modified_by_one_byte_is_refused() {
        let mut setup = Setup::new("1.0.18", &sha256_hex(INSTALLER));
        setup.sig = Sig::Tampered;
        let served = serve_with(setup);
        let (updater, dir) = updater_for(&served, "1.0.17", "tampered");
        let error = updater.autoupdate().unwrap_err();
        assert!(
            error.to_string().contains("signature is invalid"),
            "{}",
            error
        );
        assert_eq!(
            served.hits.load(Ordering::SeqCst),
            2,
            "installer not requested"
        );
        assert_nothing_left(&dir);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn signature_by_another_key_is_refused() {
        let mut setup = Setup::new("1.0.18", &sha256_hex(INSTALLER));
        setup.sig = Sig::OtherKey;
        let served = serve_with(setup);
        let (updater, dir) = updater_for(&served, "1.0.17", "other-key");
        let error = updater.autoupdate().unwrap_err();
        assert!(
            error.to_string().contains("signature is invalid"),
            "{}",
            error
        );
        assert_nothing_left(&dir);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_signature_file_is_refused() {
        let mut setup = Setup::new("1.0.18", &sha256_hex(INSTALLER));
        setup.sig = Sig::Missing;
        let served = serve_with(setup);
        let (updater, dir) = updater_for(&served, "1.0.17", "no-sig");
        let error = updater.autoupdate().unwrap_err();
        assert!(format!("{:#}", error).contains("signature"), "{:#}", error);
        assert_eq!(
            served.hits.load(Ordering::SeqCst),
            2,
            "installer not requested"
        );
        assert_nothing_left(&dir);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn malformed_public_key_refuses_any_update_without_a_request() {
        for bad in ["", "zz", &"g".repeat(64), &"00".repeat(31)] {
            let served = serve("1.0.18", &sha256_hex(INSTALLER));
            let (updater, dir) = updater_at(&served.endpoint, bad, "1.0.17", "bad-key");
            let error = updater.autoupdate().unwrap_err();
            assert!(error.to_string().contains("public key"), "{}", error);
            assert_eq!(served.hits.load(Ordering::SeqCst), 0);
            assert_nothing_left(&dir);
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn garbage_signature_is_refused() {
        for sig in [Sig::NotBase64, Sig::WrongLength] {
            let mut setup = Setup::new("1.0.18", &sha256_hex(INSTALLER));
            setup.sig = sig;
            let served = serve_with(setup);
            let (updater, dir) = updater_for(&served, "1.0.17", "garbage-sig");
            let error = updater.autoupdate().unwrap_err();
            assert!(format!("{:#}", error).contains("signature"), "{:#}", error);
            assert_eq!(served.hits.load(Ordering::SeqCst), 2);
            assert_nothing_left(&dir);
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn host_outside_the_list_is_refused_without_any_request() {
        let served = serve("1.0.18", &sha256_hex(INSTALLER));
        let (mut updater, dir) = updater_for(&served, "1.0.17", "host");
        updater.allowed_hosts = vec!["laststream.gtux-prog.workers.dev".to_string()];
        let error = updater.autoupdate().unwrap_err();
        assert!(error.to_string().contains("not allowed"), "{}", error);
        assert_eq!(served.hits.load(Ordering::SeqCst), 0);
        assert_nothing_left(&dir);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn installer_on_a_host_outside_the_list_is_refused() {
        let mut setup = Setup::new("1.0.18", &sha256_hex(INSTALLER));
        setup.installer_base = Some("https://evil.invalid");
        let served = serve_with(setup);
        let (updater, dir) = updater_for(&served, "1.0.17", "installer-host");
        let error = updater.autoupdate().unwrap_err();
        assert!(error.to_string().contains("not allowed"), "{}", error);
        assert_eq!(
            served.hits.load(Ordering::SeqCst),
            2,
            "no installer request"
        );
        assert_nothing_left(&dir);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn host_check_is_exact_and_case_insensitive() {
        let allowed = vec!["laststream.gtux-prog.workers.dev".to_string()];
        let ok = Url::parse("https://LastStream.gtux-prog.workers.dev/x").unwrap();
        assert!(check_url(&ok, "t", &allowed).is_ok());
        for bad in [
            "https://evil.laststream.gtux-prog.workers.dev/x",
            "https://laststream.gtux-prog.workers.dev.evil.test/x",
            "https://evil.test/laststream.gtux-prog.workers.dev",
            "https://laststream.gtux-prog.workers.dev@evil.test/x",
        ] {
            let url = Url::parse(bad).unwrap();
            assert!(check_url(&url, "t", &allowed).is_err(), "{}", bad);
        }
    }

    #[test]
    fn signature_url_appends_sig_to_the_path() {
        let url = Url::parse("https://h.test/app/windows/latest.json?x=1#f").unwrap();
        assert_eq!(
            signature_url(&url).as_str(),
            "https://h.test/app/windows/latest.json.sig"
        );
    }
}
