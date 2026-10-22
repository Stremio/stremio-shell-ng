//! Pont HTTP natif « addon stream » : interroge depuis l'appareil des addons Stremio que Cloudflare bloque côté serveur.
//! Catégorie strictement bornée : GET https:443 uniquement, chemin `.../stream/<type>/<id>.json` ou `.../manifest.json`,
//! hôte nominatif (pas d'IP littérale) qui ne résout que vers des adresses publiques (vérifié après résolution DNS,
//! puis connexion épinglée sur ces adresses), sans identifiants, réponse <= 5 Mo, 10 s au total, seul `Accept` est
//! transmis, redirections (3 max) suivies seulement si la cible respecte les mêmes règles.
//! Aucune journalisation : l'URL peut contenir une clé d'addon (seul l'hôte pourrait être cité, jamais le chemin).

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::time::{Duration, Instant};

const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REDIRECTS: usize = 3;
const MAX_URL_LEN: usize = 2048;
const STREAM_TYPES: &[&str] = &["movie", "series", "anime", "tv", "channel"];

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AddonHttpRequest {
    pub request_id: u64,
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
    /// Seul `Accept` est retenu ; le reste est ignoré.
    #[serde(default)]
    pub headers: Option<std::collections::HashMap<String, String>>,
}

fn error_reply(request_id: u64, code: &str) -> Value {
    json!({ "requestId": request_id, "error": code })
}

fn id_char_ok(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '_' | '%' | '-')
}

/// Chemin se terminant par `/stream/<type>/<id>.json` ou par `/manifest.json`.
pub fn path_allowed(path: &str) -> bool {
    if path.ends_with("/manifest.json") {
        return true;
    }
    let Some(stem) = path.strip_suffix(".json") else {
        return false;
    };
    let mut parts = stem.rsplitn(3, '/');
    let (Some(id), Some(kind), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    (rest.ends_with("/stream") || rest == "stream")
        && STREAM_TYPES.contains(&kind)
        && !id.is_empty()
        && id.len() <= 200
        && id.chars().all(id_char_ok)
}

/// Adresse IP routable publiquement (refuse privé, loopback, link-local, CGNAT, réservé, documentation, multicast).
pub fn ip_is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => ipv4_is_public(v4),
        IpAddr::V6(v6) => ipv6_is_public(v6),
    }
}

fn ipv4_is_public(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || o[0] == 0
        || (o[0] == 100 && (64..=127).contains(&o[1])) // CGNAT 100.64/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24
        || (o[0] == 192 && o[1] == 0 && o[2] == 2) // TEST-NET-1
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // benchmark
        || (o[0] == 198 && o[1] == 51 && o[2] == 100) // TEST-NET-2
        || (o[0] == 203 && o[1] == 0 && o[2] == 113) // TEST-NET-3
        || o[0] >= 240) // réservé
}

fn ipv6_is_public(ip: Ipv6Addr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return false;
    }
    let s = ip.segments();
    // IPv4 encapsulé : ::ffff:a.b.c.d, ::a.b.c.d (compatible), 64:ff9b::/96 (NAT64), 2002::/16 (6to4).
    let embedded =
        |hi: u16, lo: u16| Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
    if s[..5] == [0, 0, 0, 0, 0] && (s[5] == 0xffff || s[5] == 0) {
        return ipv4_is_public(embedded(s[6], s[7]));
    }
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return ipv4_is_public(embedded(s[6], s[7]));
    }
    if s[0] == 0x2002 {
        return ipv4_is_public(embedded(s[1], s[2]));
    }
    !((s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (s[0] & 0xffc0) == 0xfec0 // site-local obsolète
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || (s[0] == 0x2001 && s[1] == 0) // Teredo
        || (s[0] == 0x0100 && s[1..4] == [0, 0, 0])) // discard 100::/64
}

/// Hôte nominatif, https:443 sans identifiants (la forme d'URL commune aux ponts natifs vers un hôte public arbitraire).
/// Pas de contrôle du chemin ni de la requête ; la résolution DNS publique reste à faire (`resolve_public`).
pub fn public_named_url(raw: &str, max_len: usize) -> Option<url::Url> {
    if raw.len() > max_len {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    let domain = match url.host()? {
        url::Host::Domain(d) => d.to_ascii_lowercase(),
        _ => return None, // IP littérale (v4 ou v6)
    };
    let host_ok = domain.len() <= 253
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        // Une IPv4 en décimal/octal/hex est normalisée en Host::Ipv4 par `url` ; on refuse aussi un dernier label numérique.
        && !domain.rsplit('.').next().map_or(true, |l| l.chars().all(|c| c.is_ascii_digit()));
    let ok = host_ok
        && url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(url.port(), None | Some(443));
    ok.then_some(url)
}

/// Valide l'URL sans réseau : https, port 443, pas d'identifiants, hôte nominatif, chemin borné.
pub fn parse_addon_url(raw: &str) -> Option<url::Url> {
    let url = public_named_url(raw, MAX_URL_LEN)?;
    let ok = url.query().is_none() && url.fragment().is_none() && path_allowed(url.path());
    ok.then_some(url)
}

/// Cible d'une redirection : doit respecter exactement les mêmes règles que la requête initiale.
pub fn redirect_target(base: &url::Url, location: &str) -> Option<url::Url> {
    let next = base.join(location).ok()?;
    parse_addon_url(next.as_str())
}

/// Résout l'hôte et refuse si UNE adresse n'est pas publique. Renvoie les adresses à épingler.
pub fn resolve_public(
    host: &str,
    resolver: &dyn Fn(&str) -> Vec<IpAddr>,
) -> Option<Vec<SocketAddr>> {
    resolve_public_checked(host, resolver).ok()
}

/// Comme `resolve_public`, mais distingue un échec de résolution (`network` : aucune adresse, DNS indisponible) d'un refus
/// (`host_not_allowed` : au moins une adresse non publique).
pub fn resolve_public_checked(
    host: &str,
    resolver: &dyn Fn(&str) -> Vec<IpAddr>,
) -> Result<Vec<SocketAddr>, &'static str> {
    let ips = resolver(host);
    if ips.is_empty() {
        return Err("network");
    }
    if !ips.iter().all(|ip| ip_is_public(*ip)) {
        return Err("host_not_allowed");
    }
    Ok(ips.into_iter().map(|ip| SocketAddr::new(ip, 443)).collect())
}

/// Résolution système ; un échec (liste vide) est retenté une fois avant d'abandonner.
pub fn system_resolver(host: &str) -> Vec<IpAddr> {
    for _ in 0..2 {
        let ips: Vec<IpAddr> = (host, 443u16)
            .to_socket_addrs()
            .map(|it| it.map(|a| a.ip()).collect())
            .unwrap_or_default();
        if !ips.is_empty() {
            return ips;
        }
    }
    Vec::new()
}

/// Lit au plus `max` octets ; `Err(true)` si la source en contient davantage, `Err(false)` en cas d'erreur de lecture.
fn read_limited<R: std::io::Read>(reader: R, max: usize) -> Result<Vec<u8>, bool> {
    use std::io::Read;
    let mut bytes = Vec::new();
    reader
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| false)?;
    if bytes.len() > max {
        return Err(true);
    }
    Ok(bytes)
}

/// Exécute la requête (bloquant : à lancer dans un fil). Renvoie la charge du signal `native-http-response`.
pub fn run(req: &AddonHttpRequest) -> Value {
    run_with(req, &system_resolver)
}

fn run_with(req: &AddonHttpRequest, resolver: &dyn Fn(&str) -> Vec<IpAddr>) -> Value {
    if !req
        .method
        .as_deref()
        .unwrap_or("GET")
        .eq_ignore_ascii_case("GET")
    {
        return error_reply(req.request_id, "method_not_allowed");
    }
    let mut url = match parse_addon_url(&req.url) {
        Some(url) => url,
        None => return error_reply(req.request_id, "host_not_allowed"),
    };
    let accept = req
        .headers
        .as_ref()
        .and_then(|h| {
            h.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("accept"))
                .map(|(_, v)| v.clone())
        })
        .filter(|v| v.len() <= 200 && v.chars().all(|c| c.is_ascii() && !c.is_ascii_control()))
        .unwrap_or_else(|| "application/json".to_owned());
    let started = Instant::now();
    for hop in 0..=MAX_REDIRECTS {
        let remaining = match TOTAL_TIMEOUT.checked_sub(started.elapsed()) {
            Some(r) if !r.is_zero() => r,
            _ => return error_reply(req.request_id, "timeout"),
        };
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let addrs = match resolve_public(&host, resolver) {
            Some(a) => a,
            None => return error_reply(req.request_id, "host_not_allowed"),
        };
        // Connexion épinglée sur les adresses vérifiées (pas de second DNS exploitable), sans suivi automatique.
        let client = match reqwest::blocking::Client::builder()
            .timeout(remaining)
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(&host, &addrs)
            .build()
        {
            Ok(c) => c,
            Err(_) => return error_reply(req.request_id, "client"),
        };
        let response = match client
            .get(url.clone())
            .header("Accept", accept.as_str())
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
        if response
            .content_length()
            .map_or(false, |n| n > MAX_RESPONSE_BYTES as u64)
        {
            return error_reply(req.request_id, "too_large");
        }
        let mut headers = serde_json::Map::new();
        if let Some(ct) = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
        {
            headers.insert("content-type".to_owned(), Value::String(ct.to_owned()));
        }
        let bytes = match read_limited(response, MAX_RESPONSE_BYTES) {
            Ok(b) => b,
            Err(true) => return error_reply(req.request_id, "too_large"),
            Err(false) => return error_reply(req.request_id, "network"),
        };
        return json!({
            "requestId": req.request_id,
            "status": status.as_u16(),
            "headers": headers,
            "bodyBase64": base64::engine::general_purpose::STANDARD.encode(&bytes),
        });
    }
    error_reply(req.request_id, "redirect_not_allowed")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(url: &str) -> AddonHttpRequest {
        AddonHttpRequest {
            request_id: 1,
            url: url.into(),
            method: None,
            headers: None,
        }
    }

    #[test]
    fn accepts_stream_and_manifest_urls() {
        for ok in [
            "https://addon.example.com/stream/movie/tt0133093.json",
            "https://addon.example.com/realdebrid=XXXX|lang=french/stream/movie/tt0133093.json",
            "https://addon.example.com/eyJhIjoiYiJ9/stream/series/tt0944947:1:2.json",
            "https://addon.example.com/3f8a1c6e-7b2d-4e95-a0f4-6d2c9b1e7a58/stream/anime/kitsu:1234:5.json",
            "https://addon.example.com/stream/tv/some%20id.json",
            "https://addon.example.com/stream/channel/abc_1.json",
            "https://addon.example.com/manifest.json",
            "https://addon.example.com/cfg/manifest.json",
            "https://addon.example.com:443/manifest.json",
        ] {
            assert!(parse_addon_url(ok).is_some(), "{}", ok);
        }
    }

    #[test]
    fn refuses_bad_scheme_credentials_port_query() {
        for ko in [
            "http://addon.example.com/manifest.json",
            "ftp://addon.example.com/manifest.json",
            "file:///c:/x/manifest.json",
            "https://user:pw@addon.example.com/manifest.json",
            "https://user@addon.example.com/manifest.json",
            "https://addon.example.com:8443/manifest.json",
            "https://addon.example.com/manifest.json?x=1",
            "https://addon.example.com/manifest.json#a",
            "https://localhost/manifest.json",
            "not a url",
        ] {
            assert!(parse_addon_url(ko).is_none(), "{}", ko);
        }
    }

    #[test]
    fn refuses_bad_paths() {
        for ko in [
            "https://addon.example.com/",
            "https://addon.example.com/catalog/movie/top.json",
            "https://addon.example.com/stream/movie/tt1",
            "https://addon.example.com/stream/movie/tt1.json.exe",
            "https://addon.example.com/stream/book/tt1.json",
            "https://addon.example.com/stream/movie/.json",
            "https://addon.example.com/stream/movie/a;b.json",
            "https://addon.example.com/stream/movie/a/b.json",
            "https://addon.example.com/xstream/movie/tt1.json",
            "https://addon.example.com/movie/tt1.json",
            "https://addon.example.com/config.json",
        ] {
            assert!(parse_addon_url(ko).is_none(), "{}", ko);
        }
        let long = format!(
            "https://addon.example.com/stream/movie/{}.json",
            "a".repeat(201)
        );
        assert!(parse_addon_url(&long).is_none());
        let max = format!(
            "https://addon.example.com/stream/movie/{}.json",
            "a".repeat(200)
        );
        assert!(parse_addon_url(&max).is_some());
    }

    #[test]
    fn refuses_literal_ips() {
        for ko in [
            "https://127.0.0.1/manifest.json",
            "https://8.8.8.8/manifest.json",
            "https://[::1]/manifest.json",
            "https://[2606:4700::1111]/manifest.json",
            "https://2130706433/manifest.json",
            "https://0x7f.1/manifest.json",
            "https://192.168.1.10/stream/movie/tt1.json",
        ] {
            assert!(parse_addon_url(ko).is_none(), "{}", ko);
        }
    }

    #[test]
    fn classifies_ip_addresses() {
        for ko in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.5.4",
            "192.168.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "198.18.0.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::7f00:1",
            "2002:c0a8:0101::1",
            "2001:db8::1",
            "ff02::1",
        ] {
            assert!(!ip_is_public(ko.parse().unwrap()), "{}", ko);
        }
        for ok in [
            "8.8.8.8",
            "1.1.1.1",
            "172.67.1.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(ip_is_public(ok.parse().unwrap()), "{}", ok);
        }
    }

    #[test]
    fn private_resolution_is_refused() {
        let private = |_: &str| vec!["10.1.2.3".parse::<IpAddr>().unwrap()];
        let mixed = |_: &str| {
            vec![
                "8.8.8.8".parse::<IpAddr>().unwrap(),
                "127.0.0.1".parse::<IpAddr>().unwrap(),
            ]
        };
        let none = |_: &str| Vec::<IpAddr>::new();
        let u = "https://addon.example.com/manifest.json";
        assert_eq!(run_with(&req(u), &private)["error"], "host_not_allowed");
        assert_eq!(run_with(&req(u), &mixed)["error"], "host_not_allowed");
        assert_eq!(run_with(&req(u), &none)["error"], "host_not_allowed");
        assert!(resolve_public("a.example.com", &|_| vec!["8.8.8.8".parse().unwrap()]).is_some());
    }

    #[test]
    fn refuses_method_and_url_without_network() {
        let boom = |_: &str| -> Vec<IpAddr> { panic!("no DNS expected") };
        let mut r = req("https://addon.example.com/manifest.json");
        r.method = Some("POST".into());
        assert_eq!(run_with(&r, &boom)["error"], "method_not_allowed");
        assert_eq!(
            run_with(&req("http://addon.example.com/manifest.json"), &boom)["error"],
            "host_not_allowed"
        );
        assert_eq!(
            run_with(&req("https://10.0.0.1/manifest.json"), &boom)["error"],
            "host_not_allowed"
        );
    }

    #[test]
    fn redirects_follow_same_rules() {
        let base = url::Url::parse("https://a.example.com/cfg/stream/movie/tt1.json").unwrap();
        assert!(
            redirect_target(&base, "https://b.example.org/other/stream/movie/tt1.json").is_some()
        );
        assert!(redirect_target(&base, "/x/stream/movie/tt2.json").is_some());
        assert!(redirect_target(&base, "http://b.example.org/stream/movie/tt1.json").is_none());
        assert!(redirect_target(&base, "https://127.0.0.1/stream/movie/tt1.json").is_none());
        assert!(redirect_target(&base, "https://b.example.org/download/file.mp4").is_none());
        assert!(redirect_target(&base, "https://b.example.org:8443/manifest.json").is_none());
        assert!(redirect_target(&base, "https://u:p@b.example.org/manifest.json").is_none());
        assert!(redirect_target(&base, "https://api.alldebrid.com/v4/user").is_none());
    }

    #[test]
    fn response_size_is_limited() {
        assert_eq!(read_limited(&[1u8; 10][..], 10).unwrap().len(), 10);
        assert_eq!(read_limited(&[1u8; 11][..], 10), Err(true));
        assert_eq!(read_limited(&[][..], 10).unwrap().len(), 0);
    }
}
