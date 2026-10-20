//! Pont HTTP « playlist de box TV » : lit depuis l'appareil la playlist M3U d'une box (Freebox) ou d'une URL M3U saisie par l'utilisateur.
//! Catégorie strictement bornée, distincte de `native-http` (débrideurs) et de `native-addon-http` (addons, hôtes publics seulement) :
//! GET uniquement ; soit exactement `http://mafreebox.freebox.fr/freeboxtv/playlist.m3u`, soit une URL http(s) saisie par
//! l'utilisateur (sans identifiants dans l'URL, sans fragment). Les hôtes privés / réseau local sont autorisés ici et seulement ici.
//! Réponse <= 20 Mo, 10 s au total, seul un `Accept` fixe est envoyé, redirections (3 max) suivies selon les mêmes règles, et le corps doit
//! commencer par `#EXTM3U` (sinon `not_playlist`) : la commande ne sert pas à lire d'autres services du réseau local.
//! Aucune journalisation : l'URL d'une playlist peut contenir des identifiants.

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const MAX_RESPONSE_BYTES: usize = 20 * 1024 * 1024;
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REDIRECTS: usize = 3;
const MAX_URL_LEN: usize = 2048;
const FREEBOX_URL: &str = "http://mafreebox.freebox.fr/freeboxtv/playlist.m3u";

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BoxPlaylistRequest {
    pub request_id: u64,
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
}

fn error_reply(request_id: u64, code: &str) -> Value {
    json!({ "requestId": request_id, "error": code })
}

/// Valide l'URL sans réseau : la playlist Freebox exacte, ou une URL http(s) sans identifiants ni fragment.
pub fn parse_box_url(raw: &str) -> Option<url::Url> {
    if raw.len() > MAX_URL_LEN || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    if url.as_str() == FREEBOX_URL {
        return Some(url);
    }
    let ok = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|h| !h.is_empty())
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none();
    ok.then_some(url)
}

/// Cible d'une redirection : mêmes règles que la requête initiale.
pub fn redirect_target(base: &url::Url, location: &str) -> Option<url::Url> {
    let next = base.join(location).ok()?;
    parse_box_url(next.as_str())
}

/// Le corps est-il une playlist M3U (BOM et espaces initiaux tolérés) ?
#[cfg(test)]
pub fn is_m3u(body: &[u8]) -> bool {
    let body = body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body);
    let start = body.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(body.len());
    body[start..].starts_with(b"#EXTM3U")
}

/// Corps admis : playlist M3U, guide XMLTV (`<?xml` ou `<tv`) ou son archive gzip (`.xml.gz`). Tout le reste (page HTML d'un routeur, JSON…) est refusé.
pub fn is_playlist_body(body: &[u8]) -> bool {
    if body.starts_with(&[0x1f, 0x8b]) {
        return true;
    }
    let body = body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body);
    let start = body.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(body.len());
    let head = &body[start..];
    head.starts_with(b"#EXTM3U") || head.starts_with(b"<?xml") || head.starts_with(b"<tv")
}

/// Lit au plus `max` octets ; `Err(true)` si la source en contient davantage, `Err(false)` en cas d'erreur de lecture.
fn read_limited<R: std::io::Read>(reader: R, max: usize) -> Result<Vec<u8>, bool> {
    use std::io::Read;
    let mut bytes = Vec::new();
    reader.take(max as u64 + 1).read_to_end(&mut bytes).map_err(|_| false)?;
    if bytes.len() > max {
        return Err(true);
    }
    Ok(bytes)
}

/// Exécute la requête (bloquant : à lancer dans un fil). Renvoie la charge du signal `native-http-response`.
pub fn run(req: &BoxPlaylistRequest) -> Value {
    if !req.method.as_deref().unwrap_or("GET").eq_ignore_ascii_case("GET") {
        return error_reply(req.request_id, "method_not_allowed");
    }
    let mut url = match parse_box_url(&req.url) {
        Some(url) => url,
        None => return error_reply(req.request_id, "host_not_allowed"),
    };
    let started = Instant::now();
    for hop in 0..=MAX_REDIRECTS {
        let remaining = match TOTAL_TIMEOUT.checked_sub(started.elapsed()) {
            Some(r) if !r.is_zero() => r,
            _ => return error_reply(req.request_id, "timeout"),
        };
        let client = match reqwest::blocking::Client::builder()
            .timeout(remaining)
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            Ok(c) => c,
            Err(_) => return error_reply(req.request_id, "client"),
        };
        let response = match client
            .get(url.clone())
            .header("Accept", "audio/x-mpegurl, application/x-mpegurl, text/plain, */*")
            .header("User-Agent", "Laststream")
            .send()
        {
            Ok(r) => r,
            Err(e) if e.is_timeout() => return error_reply(req.request_id, "timeout"),
            Err(_) => return error_reply(req.request_id, "network"),
        };
        let status = response.status();
        if status.is_redirection() {
            let target = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|loc| redirect_target(&url, loc));
            match target {
                Some(next) if hop < MAX_REDIRECTS => {
                    url = next;
                    continue;
                }
                _ => return error_reply(req.request_id, "redirect_not_allowed"),
            }
        }
        if response.content_length().map_or(false, |n| n > MAX_RESPONSE_BYTES as u64) {
            return error_reply(req.request_id, "too_large");
        }
        let bytes = match read_limited(response, MAX_RESPONSE_BYTES) {
            Ok(b) => b,
            Err(true) => return error_reply(req.request_id, "too_large"),
            Err(false) => return error_reply(req.request_id, "network"),
        };
        if status.is_success() && !is_playlist_body(&bytes) {
            return error_reply(req.request_id, "not_playlist");
        }
        return json!({
            "requestId": req.request_id,
            "status": status.as_u16(),
            "headers": {},
            "bodyBase64": base64::engine::general_purpose::STANDARD.encode(&bytes),
        });
    }
    error_reply(req.request_id, "redirect_not_allowed")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(url: &str) -> BoxPlaylistRequest {
        BoxPlaylistRequest { request_id: 1, url: url.into(), method: None }
    }

    #[test]
    fn accepts_freebox_and_user_m3u_urls() {
        for ok in [
            "http://mafreebox.freebox.fr/freeboxtv/playlist.m3u",
            "http://192.168.1.254/playlist.m3u",
            "http://box.local:8080/tv/list.m3u8",
            "https://exemple.fr/iptv.m3u?token=abc",
        ] {
            assert!(parse_box_url(ok).is_some(), "{}", ok);
        }
    }

    #[test]
    fn refuses_other_schemes_credentials_and_junk() {
        for ko in [
            "ftp://exemple.fr/p.m3u",
            "file:///c:/p.m3u",
            "rtsp://mafreebox.freebox.fr/fbxtv_pub/stream?namespace=1&service=1",
            "http://user:pw@exemple.fr/p.m3u",
            "http://user@exemple.fr/p.m3u",
            "http://exemple.fr/p.m3u#frag",
            "http://exemple.fr/a b.m3u",
            "javascript:alert(1)",
            "not a url",
            "",
        ] {
            assert!(parse_box_url(ko).is_none(), "{}", ko);
        }
        let long = format!("http://exemple.fr/{}.m3u", "a".repeat(MAX_URL_LEN));
        assert!(parse_box_url(&long).is_none());
    }

    #[test]
    fn redirects_follow_same_rules() {
        let base = url::Url::parse("http://mafreebox.freebox.fr/freeboxtv/playlist.m3u").unwrap();
        assert!(redirect_target(&base, "/autre/liste.m3u").is_some());
        assert!(redirect_target(&base, "https://exemple.fr/p.m3u").is_some());
        assert!(redirect_target(&base, "ftp://exemple.fr/p.m3u").is_none());
        assert!(redirect_target(&base, "http://u:p@exemple.fr/p.m3u").is_none());
    }

    #[test]
    fn body_must_be_m3u() {
        assert!(is_m3u(b"#EXTM3U\n#EXTINF:0,1 - TF1\n"));
        assert!(is_m3u(b"\xEF\xBB\xBF  \r\n#EXTM3U"));
        assert!(!is_m3u(b"<html>routeur</html>"));
        assert!(!is_m3u(b""));
        assert!(!is_m3u(b"{\"a\":1}"));
    }

    #[test]
    fn body_may_be_xmltv_or_gzip() {
        assert!(is_playlist_body(b"<?xml version=\"1.0\"?><tv></tv>"));
        assert!(is_playlist_body(b"  <tv generator-info-name=\"x\">"));
        assert!(is_playlist_body(&[0x1f, 0x8b, 8, 0]));
        assert!(is_playlist_body(b"#EXTM3U"));
        assert!(!is_playlist_body(b"<html><body>login</body></html>"));
        assert!(!is_playlist_body(b"{\"user_info\":{}}"));
    }

    #[test]
    fn accepts_typical_user_playlist_urls() {
        for ok in [
            "http://fournisseur.example:8080/get.php?username=u&password=p&type=m3u_plus",
            "https://exemple.fr/guide/epg.xml.gz",
            "http://10.0.0.5:9981/playlist/channels.m3u",
        ] {
            assert!(parse_box_url(ok).is_some(), "{}", ok);
        }
    }

    #[test]
    fn refuses_method_and_url_without_network() {
        let mut r = req("http://mafreebox.freebox.fr/freeboxtv/playlist.m3u");
        r.method = Some("POST".into());
        assert_eq!(run(&r)["error"], "method_not_allowed");
        assert_eq!(run(&req("ftp://exemple.fr/p.m3u"))["error"], "host_not_allowed");
        assert_eq!(run(&req("rtsp://mafreebox.freebox.fr/x"))["error"], "host_not_allowed");
    }

    #[test]
    fn response_size_is_limited() {
        assert_eq!(read_limited(&[1u8; 10][..], 10).unwrap().len(), 10);
        assert_eq!(read_limited(&[1u8; 11][..], 10), Err(true));
    }
}
