//! Choice of the start page of the shell: the public Cloudflare service by default, an optional
//! local service (embedded lastAddon) or a custom URL, from `%APPDATA%\Laststream\shell.json`
//! (`{"server":"public"|"local"|"<url>"}`).

use std::{fs, io, path::PathBuf, thread, time::Duration};
use url::Url;

/// Public Laststream service (web interface and API). The only place to change for a new domain.
pub const PUBLIC_URL: &str = "https://laststream.gtux-prog.workers.dev/";
/// Timeout of each attempt of the start-up probe: long enough for a cold DNS/TLS start on the
/// first launch of a fresh executable (SmartScreen/antivirus scan), still bounded.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Number of automatic attempts before the error dialog (an offline machine fails fast on DNS).
pub const PROBE_ATTEMPTS: u32 = 3;
/// Wait before the second attempt; it doubles before each following one.
pub const PROBE_BACKOFF: Duration = Duration::from_millis(1500);

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

/// Clé de `shell.json` : dossier des téléchargements choisi par l'utilisateur (absente = dossier par défaut, ODY-201).
const DOWNLOADS_KEY: &str = "downloads_dir";

/// Dossier des téléchargements enregistré dans `shell.json`, s'il y en a un.
pub fn downloads_dir() -> Option<String> {
    let json = fs::read_to_string(config_path()?).ok()?;
    parse_downloads_dir(&json)
}

/// Lit `downloads_dir` ; clé absente, vide, non textuelle ou fichier illisible donnent `None`.
pub fn parse_downloads_dir(json: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(strip_bom(json)).ok()?;
    value
        .get(DOWNLOADS_KEY)?
        .as_str()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
}

/// Enregistre `path` comme dossier des téléchargements dans `shell.json`, les autres clés étant conservées.
pub fn save_downloads_dir(path: &str) -> io::Result<()> {
    let file = config_path()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no configuration directory"))?;
    let current = match fs::read_to_string(&file) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let updated = with_downloads_dir(current.as_deref(), path)?;
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    // Écriture dans un fichier temporaire puis remplacement : un arrêt en cours d'écriture ne tronque pas la config.
    let temp = file.with_extension("json.tmp");
    fs::write(&temp, updated)?;
    fs::rename(&temp, &file)
}

/// Contenu de `shell.json` avec `downloads_dir` mis à jour. Un fichier vide compte comme `{}` ; un fichier qui n'est
/// pas un objet JSON est refusé plutôt que remplacé, pour ne perdre aucune clé.
fn with_downloads_dir(current: Option<&str>, path: &str) -> io::Result<String> {
    let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidData, message.to_owned());
    let mut value = match current
        .map(|text| strip_bom(text).trim())
        .filter(|text| !text.is_empty())
    {
        None => serde_json::Value::Object(serde_json::Map::new()),
        Some(text) => serde_json::from_str::<serde_json::Value>(text)
            .map_err(|error| invalid(&format!("shell.json is not valid JSON: {error}")))?,
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| invalid("shell.json is not a JSON object"))?;
    object.insert(
        DOWNLOADS_KEY.to_owned(),
        serde_json::Value::String(path.to_owned()),
    );
    serde_json::to_string_pretty(&value).map_err(|error| invalid(&error.to_string()))
}

/// Retire la marque d'ordre des octets UTF-8 qu'écrit parfois un éditeur Windows.
fn strip_bom(text: &str) -> &str {
    text.trim_start_matches('\u{feff}')
}

/// Cause of a failed attempt, in French (it is shown in the error dialog).
fn describe_failure(is_timeout: bool, is_connect: bool, chain: &str) -> String {
    let lower = chain.to_lowercase();
    let cause = if is_timeout || lower.contains("timed out") {
        "délai dépassé"
    } else if lower.contains("dns")
        || lower.contains("resolve")
        || lower.contains("no such host")
        || lower.contains("name or service not known")
        || lower.contains("nodename")
    {
        "résolution DNS impossible"
    } else if lower.contains("certificate")
        || lower.contains("tls")
        || lower.contains("ssl")
        || lower.contains("handshake")
    {
        "échec TLS (certificat ou poignée de main)"
    } else if lower.contains("refused") {
        "connexion refusée"
    } else if is_connect {
        "connexion impossible"
    } else {
        "erreur réseau"
    };
    format!("{cause} ({chain})")
}

/// `Display` of an error with its whole `source()` chain, which holds the real cause.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(inner) = source {
        let inner_text = inner.to_string();
        if !text.contains(&inner_text) {
            text.push_str(": ");
            text.push_str(&inner_text);
        }
        source = inner.source();
    }
    text
}

/// One GET of the health URL. Any answer below 500 means the service is reachable (the page
/// itself decides what to show); a 5xx (Cloudflare 52x, deploy in progress) and every transport
/// error fail.
fn probe_once(client: &reqwest::blocking::Client, health: &str) -> Result<(), String> {
    match client.get(health).send() {
        Ok(response) if response.status().as_u16() < 500 => Ok(()),
        Ok(response) => Err(format!("code HTTP {}", response.status().as_u16())),
        Err(err) => {
            // The URL is already shown by the dialog; its host name must not skew the classification.
            let err = err.without_url();
            Err(describe_failure(
                err.is_timeout(),
                err.is_connect(),
                &error_chain(&err),
            ))
        }
    }
}

/// Probes `{url}/health` up to `attempts` times, `timeout` per attempt, waiting `backoff` then
/// twice as long between attempts. When every attempt fails, the error holds the cause of the last.
pub fn probe(url: &str, timeout: Duration, attempts: u32, backoff: Duration) -> Result<(), String> {
    let health = format!("{}/health", url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|err| format!("client HTTP indisponible ({})", error_chain(&err)))?;
    let attempts = attempts.max(1);
    let mut wait = backoff;
    let mut last = String::new();
    for attempt in 1..=attempts {
        match probe_once(&client, &health) {
            Ok(()) => return Ok(()),
            Err(cause) => last = cause,
        }
        if attempt < attempts {
            thread::sleep(wait);
            wait = wait.saturating_mul(2);
        }
    }
    Err(format!("{last}, après {attempts} tentative(s)"))
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

    /// Answers one connection per entry of `statuses`, then closes the listener (the port refuses).
    fn serve(statuses: &'static [u16]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            for status in statuses {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut buf = [0u8; 512];
                    stream.read(&mut buf).ok();
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                    );
                    stream.write_all(reply.as_bytes()).ok();
                }
            }
        });
        url
    }

    const FAST: Duration = Duration::from_millis(20);

    #[test]
    fn downloads_dir_is_optional_and_read_leniently() {
        assert_eq!(parse_downloads_dir(""), None);
        assert_eq!(parse_downloads_dir("pas du json"), None);
        assert_eq!(parse_downloads_dir("{}"), None);
        assert_eq!(parse_downloads_dir(r#"{"server":"local"}"#), None);
        assert_eq!(parse_downloads_dir(r#"{"downloads_dir":""}"#), None);
        assert_eq!(parse_downloads_dir(r#"{"downloads_dir":3}"#), None);
        assert_eq!(
            parse_downloads_dir(r#"{"downloads_dir":"  D:\\Films  "}"#),
            Some(r"D:\Films".to_owned())
        );
    }

    #[test]
    fn saving_the_folder_keeps_the_other_keys() {
        let saved =
            with_downloads_dir(Some(r#"{"server":"local","autre":{"a":1}}"#), r"D:\Films").unwrap();
        assert_eq!(parse_downloads_dir(&saved), Some(r"D:\Films".to_owned()));
        assert_eq!(parse(&saved), Target::Local);
        let value: serde_json::Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(value["autre"]["a"], 1);
        // Un second enregistrement remplace la valeur sans rien perdre.
        let again = with_downloads_dir(Some(saved.as_str()), r"E:\Autre").unwrap();
        assert_eq!(parse_downloads_dir(&again), Some(r"E:\Autre".to_owned()));
        assert_eq!(parse(&again), Target::Local);
    }

    #[test]
    fn saving_creates_the_key_in_a_missing_or_empty_file() {
        for current in [None, Some(""), Some("  \n")] {
            let saved = with_downloads_dir(current, r"C:\Dossier").unwrap();
            assert_eq!(parse_downloads_dir(&saved), Some(r"C:\Dossier".to_owned()));
            assert_eq!(parse(&saved), Target::Public);
        }
    }

    #[test]
    fn saving_never_overwrites_a_file_it_cannot_read() {
        assert!(with_downloads_dir(Some("pas du json"), r"C:\D").is_err());
        assert!(with_downloads_dir(Some("[1, 2]"), r"C:\D").is_err());
    }

    #[test]
    fn a_byte_order_mark_is_tolerated() {
        let text = "\u{feff}{\"server\":\"local\"}";
        assert_eq!(parse_downloads_dir(text), None);
        let saved = with_downloads_dir(Some(text), r"C:\D").unwrap();
        assert_eq!(parse(&saved), Target::Local);
        assert_eq!(parse_downloads_dir(&saved), Some(r"C:\D".to_owned()));
    }

    #[test]
    fn probe_succeeds_on_the_first_answer() {
        let url = serve(&[200]);
        assert_eq!(probe(&url, Duration::from_secs(3), 3, FAST), Ok(()));
    }

    #[test]
    fn probe_recovers_from_a_transient_failure() {
        let url = serve(&[503, 200]);
        assert_eq!(probe(&url, Duration::from_secs(3), 3, FAST), Ok(()));
    }

    #[test]
    fn probe_accepts_a_client_error_status() {
        // A custom server without /health still serves the page.
        let url = serve(&[404]);
        assert_eq!(probe(&url, Duration::from_secs(3), 1, FAST), Ok(()));
    }

    #[test]
    fn probe_reports_the_http_status_after_all_attempts() {
        let url = serve(&[502, 502]);
        let err = probe(&url, Duration::from_secs(3), 2, FAST).unwrap_err();
        assert!(err.contains("code HTTP 502"), "{}", err);
        assert!(err.contains("2 tentative"), "{}", err);
    }

    #[test]
    fn probe_fails_when_nothing_listens() {
        let url = serve(&[]);
        thread::sleep(Duration::from_millis(100));
        let err = probe(&url, Duration::from_millis(800), 2, FAST).unwrap_err();
        assert!(err.contains("2 tentative"), "{}", err);
        // Windows reports a closed port after ~2 s of SYN retries: a timeout here, a refusal elsewhere.
        assert!(
            ["délai dépassé", "connexion refusée", "connexion impossible"]
                .iter()
                .any(|cause| err.starts_with(cause)),
            "{}",
            err
        );
    }

    #[test]
    fn probe_waits_between_attempts_with_a_growing_delay() {
        let url = serve(&[]);
        thread::sleep(Duration::from_millis(100));
        let started = std::time::Instant::now();
        probe(
            &url,
            Duration::from_millis(800),
            3,
            Duration::from_millis(100),
        )
        .unwrap_err();
        // 100 ms then 200 ms between three attempts.
        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    #[test]
    fn probe_reports_a_timeout() {
        // Accepts connections (backlog) but never answers.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let err = probe(&url, Duration::from_millis(200), 1, FAST).unwrap_err();
        assert!(err.starts_with("délai dépassé"), "{}", err);
    }

    #[test]
    fn probe_classification_ignores_the_url() {
        // The server closes the connection without a valid HTTP answer; the path holds words that
        // would read as a DNS or TLS cause if the URL leaked into the classification.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/ssl-dns-tls", listener.local_addr().unwrap());
        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 512];
                stream.read(&mut buf).ok();
                stream.write_all(b"garbage\r\n\r\n").ok();
            }
        });
        let err = probe(&url, Duration::from_secs(3), 1, FAST).unwrap_err();
        assert!(!err.contains("ssl-dns-tls"), "{}", err);
        assert!(err.starts_with("erreur réseau"), "{}", err);
    }

    #[test]
    fn failure_causes_are_classified() {
        assert!(describe_failure(true, true, "x").starts_with("délai dépassé"));
        assert!(
            describe_failure(false, true, "dns error: failed to lookup address")
                .starts_with("résolution DNS impossible")
        );
        assert!(
            describe_failure(false, true, "invalid peer certificate: UnknownIssuer")
                .starts_with("échec TLS")
        );
        assert!(describe_failure(
            false,
            true,
            "tcp connect error: Connection refused (os error 10061)"
        )
        .starts_with("connexion refusée"));
        assert!(
            describe_failure(false, true, "tcp connect error: unreachable")
                .starts_with("connexion impossible")
        );
        assert!(describe_failure(false, false, "connection closed").starts_with("erreur réseau"));
    }
}
