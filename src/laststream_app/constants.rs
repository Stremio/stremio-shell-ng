use url::Url;

pub const APP_NAME: &str = "Laststream";
pub const IPC_PATH: &str = "//./pipe/com.laststream.";
pub const DEV_ENDPOINT: &str = "http://127.0.0.1:11470";
pub const WINDOW_MIN_WIDTH: i32 = 1000;
pub const WINDOW_MIN_HEIGHT: i32 = 600;
pub const UPDATE_INTERVAL: u64 = 12 * 60 * 60;
/// Version feed published by `upload.ps1` (ODY-250), checked unless `--no-autoupdater` is given.
pub const DEFAULT_AUTOUPDATER_ENDPOINT: &str =
    "https://laststream.gtux-prog.workers.dev/app/windows/latest.json";
/// Hosts the update feed, the descriptor and the installer may be fetched from (and redirected to).
pub const AUTOUPDATER_ALLOWED_HOSTS: &[&str] = &["laststream.gtux-prog.workers.dev"];
/// ed25519 public key (32 bytes, hex) that must have signed `latest.json`; the signature is the
/// detached `latest.json.sig` (base64). The private key lives only in the release secret
/// `LASTSTREAM_UPDATE_SIGNING_KEY` (see `scripts/sign-update.mjs`).
pub const AUTOUPDATER_PUBLIC_KEY_HEX: &str =
    "4787c01d82cd2623d18040564ede9132347d8d9a234f1c55e85e013bcc8807d8";
pub const LASTSTREAM_SERVER_DEV_MODE: &str = "LASTSTREAM_SERVER_DEV_MODE";
pub const SRV_BUFFER_SIZE: usize = 1024;
pub const SERVER_IPC_KEY: &str = "SERVER_IPC_KEY";
pub const SRV_LOG_SIZE: usize = 20;

/// Returns the URL to hand to the default browser, or `None` when it must not be
/// opened externally. Only `http` and `https` links pass: local files, Windows
/// paths (`C:\...` parses as a one-letter scheme) and any other scheme are refused.
pub fn safe_url(uri: &str) -> Option<String> {
    let url = Url::parse(uri).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_file_urls_are_not_opened_externally() {
        for uri in [
            "file:///C:/Users/Test%20User/subtitle.srt",
            "file:///D:/a/_temp/Test%20User/legenda%20%C3%A7%C3%A3o.srt",
            "FILE:///C:/Users/Test/subtitle.srt",
            "file://server/share/subtitle.srt",
        ] {
            assert_eq!(safe_url(uri), None, "{uri}");
        }
    }

    #[test]
    fn windows_paths_are_not_opened_externally() {
        for uri in [
            r"C:\Users\Test User\subtitle.srt",
            "C:/Users/Test User/legenda ção.srt",
        ] {
            assert_eq!(safe_url(uri), None, "{uri}");
        }
    }

    #[test]
    fn http_links_open_directly() {
        assert_eq!(
            safe_url("https://www.example.com/"),
            Some("https://www.example.com/".to_string())
        );
        assert_eq!(
            safe_url("https://www.themoviedb.org/movie/603?language=fr"),
            Some("https://www.themoviedb.org/movie/603?language=fr".to_string())
        );
        assert_eq!(
            safe_url("http://example.com/page?a=1"),
            Some("http://example.com/page?a=1".to_string())
        );
    }

    #[test]
    fn non_http_schemes_are_not_opened() {
        for uri in [
            "data:text/plain,hello",
            "javascript:alert(1)",
            "ftp://example.com/file",
            "ipfs://bafy",
            "rtp://example.com",
            "calc",
        ] {
            assert_eq!(safe_url(uri), None, "{uri}");
        }
    }

    #[test]
    fn invalid_urls_are_not_opened() {
        assert_eq!(safe_url("not a url"), None);
    }

    #[test]
    fn single_instance_pipe_is_not_shared_with_other_apps() {
        assert_eq!(IPC_PATH, "//./pipe/com.laststream.");
    }
}
