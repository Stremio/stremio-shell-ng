//! Pont HTTP natif pour les débrideurs : la page ne peut pas toujours appeler leurs API (CORS) ni lire leurs fichiers.
//! Les requêtes partent de cet appareil (IP résidentielle), jamais d'un serveur. Domaines et en-têtes sont limités ;
//! rien n'est journalisé (ni URL, ni clé, ni corps).

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

/// Hôtes autorisés : API AllDebrid et TorBox, plus leurs domaines de fichiers (suffixes).
const ALLOWED_HOSTS: &[&str] = &["api.alldebrid.com", "api.torbox.app"];
const ALLOWED_SUFFIXES: &[&str] = &[
    ".debrid.it",
    ".alldebrid.com",
    ".torbox.app",
    ".real-debrid.com",
];
/// En-têtes de requête transmis (tout le reste est ignoré).
const ALLOWED_REQUEST_HEADERS: &[&str] = &["authorization", "content-type", "range", "accept"];
/// En-têtes de réponse renvoyés à la page.
pub const RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "content-range",
    "content-length",
    "retry-after",
    "accept-ranges",
];
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 20_000;
const MAX_TIMEOUT_MS: u64 = 60_000;

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NativeHttpRequest {
    pub request_id: u64,
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers: Option<std::collections::HashMap<String, String>>,
    /// Corps texte (formulaire, JSON).
    #[serde(default)]
    pub body: Option<String>,
    /// Corps binaire (multipart) encodé en base64.
    #[serde(default)]
    pub body_base64: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Vrai si `host` est un domaine de débrideur autorisé. Un suffixe ne couvre jamais le domaine nu.
pub fn host_allowed(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if host.is_empty()
        || host.len() > 253
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return false;
    }
    ALLOWED_HOSTS.contains(&host.as_str())
        || ALLOWED_SUFFIXES
            .iter()
            .any(|s| host.ends_with(s) && host.len() > s.len())
}

/// URL https d'un hôte autorisé, sans identifiants ni port autre que 443.
pub fn parse_allowed_url(raw: &str) -> Option<url::Url> {
    let url = url::Url::parse(raw).ok()?;
    let ok = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(url.port(), None | Some(443))
        && url.host_str().map(host_allowed).unwrap_or(false);
    ok.then_some(url)
}

fn allowed_method(method: &str) -> Option<reqwest::Method> {
    match method.to_ascii_uppercase().as_str() {
        "GET" => Some(reqwest::Method::GET),
        "POST" => Some(reqwest::Method::POST),
        "DELETE" => Some(reqwest::Method::DELETE),
        "HEAD" => Some(reqwest::Method::HEAD),
        _ => None,
    }
}

fn error_reply(request_id: u64, code: &str) -> Value {
    json!({ "requestId": request_id, "error": code })
}

/// Exécute la requête (bloquant : à lancer dans un fil). Renvoie la charge du signal `native-http-response`.
pub fn run(req: &NativeHttpRequest) -> Value {
    let url = match parse_allowed_url(&req.url) {
        Some(url) => url,
        None => return error_reply(req.request_id, "host_not_allowed"),
    };
    let method = match allowed_method(req.method.as_deref().unwrap_or("GET")) {
        Some(m) => m,
        None => return error_reply(req.request_id, "method_not_allowed"),
    };
    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .min(MAX_TIMEOUT_MS),
    );
    // Redirections suivies seulement vers un hôte autorisé (3 au plus).
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        let allowed = attempt.url().scheme() == "https"
            && attempt.url().host_str().map(host_allowed).unwrap_or(false);
        if attempt.previous().len() >= 3 || !allowed {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });
    let client = match reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(policy)
        .build()
    {
        Ok(c) => c,
        Err(_) => return error_reply(req.request_id, "client"),
    };
    let mut builder = client.request(method, url);
    if let Some(headers) = &req.headers {
        for (name, value) in headers {
            if ALLOWED_REQUEST_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
                builder = builder.header(name.as_str(), value.as_str());
            }
        }
    }
    if let Some(b64) = &req.body_base64 {
        match base64::engine::general_purpose::STANDARD.decode(b64) {
            Ok(bytes) if bytes.len() <= MAX_REQUEST_BYTES => builder = builder.body(bytes),
            _ => return error_reply(req.request_id, "bad_body"),
        }
    } else if let Some(text) = &req.body {
        if text.len() > MAX_REQUEST_BYTES {
            return error_reply(req.request_id, "bad_body");
        }
        builder = builder.body(text.clone());
    }
    let response = match builder.send() {
        Ok(r) => r,
        Err(e) if e.is_timeout() => return error_reply(req.request_id, "timeout"),
        Err(_) => return error_reply(req.request_id, "network"),
    };
    let status = response.status().as_u16();
    let mut headers = serde_json::Map::new();
    for name in RESPONSE_HEADERS {
        if let Some(value) = response.headers().get(*name).and_then(|v| v.to_str().ok()) {
            headers.insert((*name).to_owned(), Value::String(value.to_owned()));
        }
    }
    let mut bytes = Vec::new();
    use std::io::Read;
    if response
        .take(MAX_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return error_reply(req.request_id, "network");
    }
    if bytes.len() > MAX_RESPONSE_BYTES {
        return error_reply(req.request_id, "too_large");
    }
    json!({
        "requestId": req.request_id,
        "status": status,
        "headers": headers,
        "bodyBase64": base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_limited_to_debrid_domains() {
        for ok in [
            "api.alldebrid.com",
            "e1.debrid.it",
            "x.alldebrid.com",
            "api.torbox.app",
            "store-1.eu.torbox.app",
            "api.real-debrid.com",
            "20-4.download.real-debrid.com",
        ] {
            assert!(host_allowed(ok), "{}", ok);
        }
        for ko in [
            "debrid.it",
            "evil.com",
            "alldebrid.com.evil.com",
            "torbox.app",
            "real-debrid.com",
            "a.debrid.it.evil.com",
            "127.0.0.1",
            "",
            "api.alldebrid.com@evil.com",
        ] {
            assert!(!host_allowed(ko), "{}", ko);
        }
    }

    #[test]
    fn urls_must_be_plain_https() {
        assert!(parse_allowed_url("https://api.alldebrid.com/v4/user?agent=laststream").is_some());
        assert!(parse_allowed_url("http://api.alldebrid.com/v4/user").is_none());
        assert!(parse_allowed_url("https://user:pw@api.alldebrid.com/").is_none());
        assert!(parse_allowed_url("https://api.alldebrid.com:8443/").is_none());
        assert!(parse_allowed_url("https://evil.com/api.alldebrid.com").is_none());
        assert!(parse_allowed_url("file:///c:/windows/win.ini").is_none());
    }

    #[test]
    fn refuses_foreign_hosts_and_methods_without_network() {
        let mut req = NativeHttpRequest {
            request_id: 7,
            url: "https://evil.com/".into(),
            method: None,
            headers: None,
            body: None,
            body_base64: None,
            timeout_ms: None,
        };
        assert_eq!(run(&req)["error"], "host_not_allowed");
        req.url = "https://api.torbox.app/v1/api/user/me".into();
        req.method = Some("PUT".into());
        assert_eq!(run(&req)["error"], "method_not_allowed");
    }
}
