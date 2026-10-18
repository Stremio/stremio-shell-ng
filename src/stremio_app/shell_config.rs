//! Choice of the start page of the shell: the public Cloudflare service by default, an optional
//! local service (embedded lastAddon) or a custom URL, from `%APPDATA%\Laststream\shell.json`
//! (`{"server":"public"|"local"|"<url>"}`).

use std::{fs, path::PathBuf, time::Duration};
use url::Url;

/// Public Laststream service (web interface and API). The only place to change for a new domain.
pub const PUBLIC_URL: &str = "https://laststream.gtux-prog.workers.dev/";
/// Short timeout of the start-up probe, so that an offline machine gets the error dialog quickly.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The public service, no local process.
    Public,
    /// The embedded lastAddon on 127.0.0.1 (probe of port 7000, then bundled node).
    Local,
    /// Another remote service chosen by the user.
    Custom(String),
}

/// What the shell starts for a given target.
#[derive(Debug, PartialEq, Eq)]
pub struct StartupPlan {
    /// Launch (or reuse) the embedded lastAddon.
    pub local_addon: bool,
    /// Launch the Stremio streaming server (`stremio-runtime`).
    pub streaming_server: bool,
    /// Remote page to probe, then load.
    pub remote_url: Option<String>,
}

impl Target {
    /// `explicit_webui`: the user passed `--webui-url`/`--development`/`--staging`; the former
    /// Stremio web flow (and its streaming server) is then kept as is.
    pub fn plan(&self, explicit_webui: bool) -> StartupPlan {
        if explicit_webui {
            return StartupPlan {
                local_addon: false,
                streaming_server: true,
                remote_url: None,
            };
        }
        match self {
            Target::Local => StartupPlan {
                local_addon: true,
                streaming_server: true,
                remote_url: None,
            },
            Target::Public => StartupPlan {
                local_addon: false,
                streaming_server: false,
                remote_url: Some(PUBLIC_URL.to_owned()),
            },
            Target::Custom(url) => StartupPlan {
                local_addon: false,
                streaming_server: false,
                remote_url: Some(url.clone()),
            },
        }
    }
}

/// Reads `{"server": ...}`; anything unusable falls back to the public service.
pub fn parse(json: &str) -> Target {
    let value = serde_json::from_str::<serde_json::Value>(json).ok();
    let server = value
        .as_ref()
        .and_then(|value| value.get("server"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim);
    match server {
        Some(server) if server.eq_ignore_ascii_case("local") => Target::Local,
        Some(server) => custom_url(server).map_or(Target::Public, Target::Custom),
        None => Target::Public,
    }
}

/// An https URL, or an http URL on the loopback; `None` for "public" and everything else.
fn custom_url(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match url.scheme() {
        "https" => Some(url.to_string()),
        "http" if loopback => Some(url.to_string()),
        _ => None,
    }
}

fn config_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("Laststream").join("shell.json"))
}

pub fn load() -> Target {
    config_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .map_or(Target::Public, |json| parse(&json))
}

/// True when something answers HTTP at `url` within `timeout` (any status: the page itself
/// decides what to show). False offline, on timeout or on a refused connection.
pub fn is_reachable(url: &str, timeout: Duration) -> bool {
    let base = url.trim_end_matches('/');
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .ok()
        .is_some_and(|client| client.get(format!("{base}/health")).send().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    #[test]
    fn defaults_to_the_public_service() {
        assert_eq!(parse(""), Target::Public);
        assert_eq!(parse("not json"), Target::Public);
        assert_eq!(parse("{}"), Target::Public);
        assert_eq!(parse(r#"{"server":"public"}"#), Target::Public);
        assert_eq!(parse(r#"{"server":"ftp://x"}"#), Target::Public);
        assert_eq!(parse(r#"{"server":"http://example.com"}"#), Target::Public);
    }

    #[test]
    fn reads_local_and_custom_urls() {
        assert_eq!(parse(r#"{"server":"local"}"#), Target::Local);
        assert_eq!(parse(r#"{"server":"LOCAL"}"#), Target::Local);
        assert_eq!(
            parse(r#"{"server":"https://laststream.example.org"}"#),
            Target::Custom("https://laststream.example.org/".to_owned())
        );
        assert_eq!(
            parse(r#"{"server":"http://127.0.0.1:8787"}"#),
            Target::Custom("http://127.0.0.1:8787/".to_owned())
        );
    }

    #[test]
    fn public_mode_launches_no_node_and_no_streaming_server() {
        let plan = Target::Public.plan(false);
        assert!(!plan.local_addon);
        assert!(!plan.streaming_server);
        assert_eq!(plan.remote_url.as_deref(), Some(PUBLIC_URL));
        let plan = Target::Custom("https://a.b/".to_owned()).plan(false);
        assert!(!plan.local_addon && !plan.streaming_server);
    }

    #[test]
    fn local_mode_keeps_the_embedded_service() {
        let plan = Target::Local.plan(false);
        assert!(plan.local_addon && plan.streaming_server);
        assert_eq!(plan.remote_url, None);
    }

    #[test]
    fn explicit_webui_keeps_the_former_flow() {
        let plan = Target::Public.plan(true);
        assert!(!plan.local_addon && plan.streaming_server);
        assert_eq!(plan.remote_url, None);
    }

    #[test]
    fn probe_follows_the_reachability() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 512];
                stream.read(&mut buf).ok();
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                    .ok();
            }
        });
        assert!(is_reachable(&url, Duration::from_secs(3)));
        // The listener is gone: the port refuses connections.
        assert!(!is_reachable(&url, Duration::from_millis(800)));
    }
}
