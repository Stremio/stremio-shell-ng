//! Pont de lecture par plages « native-range » : lit depuis cet appareil un fichier débridé par tranches `Range`, sans tout charger d'un coup.
//! Deux familles d'hôtes, HTTPS:443 seulement, sans identifiants, uniquement depuis cet appareil (jamais depuis un serveur) :
//! - les débrideurs et leurs CDN (allowlist de native_http.rs) : le nom doit aussi se résoudre vers des adresses publiques
//!   (une adresse privée, loopback ou lien local refuse l'hôte), sans épinglage ni coupure du proxy système ;
//! - les liens directs d'addon, sur un hôte nominatif (pas d'IP littérale, ni `localhost`, `.local`, `.internal`, `.localhost`) qui
//!   ne résout que vers des adresses publiques (vérifié après résolution DNS, connexion épinglée sur ces adresses : ni LAN ni serveur local).
//!
//! GET uniquement, en-tête `Range: bytes=a-b` obligatoire, plage <= 8 Mo, 3 redirections au plus. Chaque saut (lien initial, puis chaque
//! `Location`) est revalidé selon les mêmes règles : schéma https, port 443, sans identifiants, hôte permis, puis résolution DNS.
//! Une cible refusée donne `redirect_not_allowed` (saut) ou `host_not_allowed` (lien initial ou résolution non publique). Côté
//! téléchargements, la page doit classer ces deux codes en `content` (lien illisible). Un 200 n'est accepté que si le corps
//! complet, depuis l'octet 0, tient dans la plage demandée ; sinon `range_unsupported`.
//!
//! Protocole (signaux vers la page, ids de requête fournis par la page) :
//! - requête `native-range` : `{id, url, headers: {range}, method?, timeoutMs?}` ;
//! - `native-range-chunk` : `{id, seq, bodyBase64, last: false}` (seq à partir de 0), émis pendant la lecture, morceaux <= 1 Mo en base64 ;
//! - `native-range-response` (toujours en dernier) : `{id, status, contentRange, contentLength, headers, chunks}` ou `{id, error, status?}` ;
//! - annulation `native-range-cancel` : `{id}` ; la lecture s'arrête et la réponse finale est `{id, error: "cancelled"}` ;
//! - capacités `native-range-caps` : répond `native-range-caps` avec `{version: 1, maxChunk, maxRange}`.
//! - refus immédiats, en `native-range-response` `{id, error}` : `busy` (6 lectures déjà actives), `duplicate_id` (id déjà actif),
//!   `spawn_failed` (fil non créé), `bad_request` (requête illisible, `id` repris s'il existe, sinon `null`).
//!
//! Rien n'est journalisé (ni URL, ni corps).

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use crate::laststream_app::addon_http::{
    public_named_url, resolve_public_checked, system_resolver,
};
use crate::laststream_app::ipc::RPCResponse;
use crate::laststream_app::native_http::{
    debrid_resolution_ok, host_allowed, parse_allowed_url, RESPONSE_HEADERS,
};
use std::net::{IpAddr, SocketAddr};
use std::time::Instant;

const VERSION: u32 = 1;
/// Taille maximale d'un morceau une fois encodé en base64.
pub const MAX_CHUNK_BASE64: usize = 1_048_576;
/// Octets bruts par morceau : 786 432 octets donnent exactement 1 048 576 caractères base64, sans bourrage.
const CHUNK_RAW_BYTES: usize = MAX_CHUNK_BASE64 / 4 * 3;
/// Plafond d'une requête : taille de la plage demandée (et donc du corps accepté).
pub const MAX_RANGE_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 20_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const MAX_REDIRECTS: usize = 3;
/// Longueur maximale d'un lien direct d'addon (les liens signés sont longs).
const MAX_DIRECT_URL_LEN: usize = 4096;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeRangeRequest {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
    /// Seul `Range` est transmis ; le reste est ignoré.
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeRangeCancel {
    pub id: u64,
}

/// Plage d'octets demandée, bornes incluses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub fn size(&self) -> u64 {
        self.end - self.start + 1
    }
}

/// En-tête `Range` strict : `bytes=a-b` avec a <= b, sans plage ouverte, suffixe ni liste.
pub fn parse_range(raw: &str) -> Option<ByteRange> {
    let spec = raw.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.len() <= 16 && s.bytes().all(|c| c.is_ascii_digit());
    if !digits(a) || !digits(b) {
        return None;
    }
    let start: u64 = a.parse().ok()?;
    let end: u64 = b.parse().ok()?;
    (start <= end).then_some(ByteRange { start, end })
}

fn requested_range(headers: Option<&HashMap<String, String>>) -> Result<ByteRange, &'static str> {
    let raw = headers
        .and_then(|h| {
            h.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("range"))
                .map(|(_, v)| v.as_str())
        })
        .ok_or("range_required")?;
    parse_range(raw).ok_or("bad_range")
}

/// Lien lisible par le pont : hôte de débrideur (allowlist) ou lien direct d'addon (hôte nominatif en https:443 ; la résolution
/// DNS publique est vérifiée à la connexion, voir `run_with`).
pub fn parse_source_url(raw: &str) -> Option<url::Url> {
    parse_allowed_url(raw).or_else(|| public_named_url(raw, MAX_DIRECT_URL_LEN))
}

/// Une redirection n'est suivie que vers une URL autorisée : mêmes règles que la requête initiale.
pub fn redirect_allowed(url: &url::Url) -> bool {
    parse_source_url(url.as_str()).is_some()
}

/// Comment se connecter à un hôte déjà validé (voir `plan_connection`).
#[derive(Debug, PartialEq, Eq)]
enum Connection {
    /// Hôte de débrideur : résolution vérifiée publique, connexion ordinaire (proxy système conservé).
    Default,
    /// Lien direct d'addon : connexion épinglée sur ces adresses publiques, sans proxy (pas de second DNS exploitable).
    Pinned(Vec<SocketAddr>),
}

/// Politique de connexion d'un saut (lien initial ou redirection), sans réseau hors du résolveur injecté.
/// Les deux familles passent par la résolution DNS : refus (`host_not_allowed`) dès qu'une adresse n'est pas publique ;
/// `network` si un lien direct ne se résout pas du tout. Les hôtes de débrideur ne sont pas épinglés (voir `debrid_resolution_ok`).
fn plan_connection(
    host: &str,
    resolver: &dyn Fn(&str) -> Vec<IpAddr>,
) -> Result<Connection, &'static str> {
    if host_allowed(host) {
        debrid_resolution_ok(host, resolver)?;
        Ok(Connection::Default)
    } else {
        resolve_public_checked(host, resolver).map(Connection::Pinned)
    }
}

/// Cible d'une redirection : l'en-tête `Location` (absolu ou relatif) doit exister, tenir dans le plafond de sauts
/// (`hop` = rang du saut qui vient de répondre) et respecter les mêmes règles que le lien initial.
fn next_hop(current: &url::Url, location: Option<&str>, hop: usize) -> Option<url::Url> {
    if hop >= MAX_REDIRECTS {
        return None;
    }
    current.join(location?).ok().filter(redirect_allowed)
}

/// Décide si la réponse peut servir la plage. 206 : oui. 200 : seulement si le corps complet (depuis l'octet 0) tient dans la plage.
pub fn check_status(
    status: u16,
    range: ByteRange,
    content_length: Option<u64>,
) -> Result<(), &'static str> {
    match status {
        206 => Ok(()),
        200 => match content_length {
            Some(len) if range.start == 0 && len <= range.end + 1 => Ok(()),
            _ => Err("range_unsupported"),
        },
        _ => Err("bad_status"),
    }
}

/// En-têtes utiles d'une réponse, avant lecture du corps.
struct ResponseHead {
    status: u16,
    content_range: Option<String>,
    content_length: Option<u64>,
    headers: serde_json::Map<String, Value>,
}

fn error_reply(id: u64, code: &str) -> Value {
    json!({ "id": id, "error": code })
}

fn error_with_status(id: u64, code: &str, status: u16) -> Value {
    json!({ "id": id, "error": code, "status": status })
}

/// Émet un morceau base64. Renvoie faux si l'annulation est demandée ou si la page ne reçoit plus rien.
fn emit(
    id: u64,
    seq: u64,
    bytes: &[u8],
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    if cancel.load(Ordering::SeqCst) {
        return false;
    }
    let body_base64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    sink(json!({ "id": id, "seq": seq, "bodyBase64": body_base64, "last": false }))
}

/// Lit le corps par morceaux bornés et les émet au fil de la lecture. Renvoie la charge finale `native-range-response`.
fn deliver<R: Read>(
    id: u64,
    range: ByteRange,
    head: ResponseHead,
    mut body: R,
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(Value) -> bool,
) -> Value {
    let status = head.status;
    let limit = range.size();
    if head.content_length.is_some_and(|n| n > limit) {
        return error_with_status(id, "too_large", status);
    }
    let mut chunk = vec![0u8; CHUNK_RAW_BYTES];
    let mut filled = 0usize;
    let mut total = 0u64;
    let mut seq = 0u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return error_with_status(id, "cancelled", status);
        }
        match body.read(&mut chunk[filled..]) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                total += n as u64;
                if total > limit {
                    return error_with_status(id, "too_large", status);
                }
                if filled == CHUNK_RAW_BYTES {
                    if !emit(id, seq, &chunk, cancel, sink) {
                        return error_with_status(id, "cancelled", status);
                    }
                    seq += 1;
                    filled = 0;
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return error_with_status(id, "network", status),
        }
    }
    if filled > 0 {
        if !emit(id, seq, &chunk[..filled], cancel, sink) {
            return error_with_status(id, "cancelled", status);
        }
        seq += 1;
    }
    json!({
        "id": id,
        "status": status,
        "contentRange": head.content_range,
        "contentLength": total,
        "headers": head.headers,
        "chunks": seq,
    })
}

/// Exécute la requête (bloquant : à lancer dans un fil). `sink` reçoit les `native-range-chunk` ; renvoie la charge finale.
pub fn run(
    req: &NativeRangeRequest,
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(Value) -> bool,
) -> Value {
    run_with(req, cancel, sink, &system_resolver)
}

fn run_with(
    req: &NativeRangeRequest,
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(Value) -> bool,
    resolver: &dyn Fn(&str) -> Vec<IpAddr>,
) -> Value {
    let id = req.id;
    if !req
        .method
        .as_deref()
        .unwrap_or("GET")
        .eq_ignore_ascii_case("GET")
    {
        return error_reply(id, "method_not_allowed");
    }
    let mut url = match parse_source_url(&req.url) {
        Some(url) => url,
        None => return error_reply(id, "host_not_allowed"),
    };
    let range = match requested_range(req.headers.as_ref()) {
        Ok(range) => range,
        Err(code) => return error_reply(id, code),
    };
    if range.size() > MAX_RANGE_BYTES {
        return error_reply(id, "too_large");
    }
    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .min(MAX_TIMEOUT_MS),
    );
    let started = Instant::now();
    // Redirections suivies à la main (3 au plus) : chaque saut est revalidé et, hors débrideur, résolu vers des adresses publiques.
    for hop in 0..=MAX_REDIRECTS {
        let remaining = match timeout.checked_sub(started.elapsed()) {
            Some(r) if !r.is_zero() => r,
            _ => return error_reply(id, "timeout"),
        };
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        // Pas de Referer automatique (aucune fuite d'URL d'origine), HTTPS obligatoire, aucun suivi automatique.
        let mut builder = reqwest::blocking::Client::builder()
            .timeout(remaining)
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .https_only(true);
        match plan_connection(&host, resolver) {
            Ok(Connection::Pinned(addrs)) => {
                builder = builder.no_proxy().resolve_to_addrs(&host, &addrs)
            }
            Ok(Connection::Default) => {}
            Err(code) => return error_reply(id, code),
        }
        let client = match builder.build() {
            Ok(c) => c,
            Err(_) => return error_reply(id, "client"),
        };
        let response = match client
            .get(url.clone())
            .header(
                reqwest::header::RANGE,
                format!("bytes={}-{}", range.start, range.end),
            )
            .send()
        {
            Ok(r) => r,
            Err(e) if e.is_timeout() => return error_reply(id, "timeout"),
            Err(_) => return error_reply(id, "network"),
        };
        let status = response.status().as_u16();
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok());
            match next_hop(&url, location, hop) {
                Some(next) => {
                    url = next;
                    continue;
                }
                None => return error_with_status(id, "redirect_not_allowed", status),
            }
        }
        let content_length = response.content_length();
        if let Err(code) = check_status(status, range, content_length) {
            return error_with_status(id, code, status);
        }
        let mut headers = serde_json::Map::new();
        for name in RESPONSE_HEADERS {
            if let Some(value) = response.headers().get(*name).and_then(|v| v.to_str().ok()) {
                headers.insert((*name).to_owned(), Value::String(value.to_owned()));
            }
        }
        let content_range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let head = ResponseHead {
            status,
            content_range,
            content_length,
            headers,
        };
        return deliver(id, range, head, response, cancel, sink);
    }
    error_with_status(id, "redirect_not_allowed", 0)
}

/// Annulations en cours, par identifiant de requête. Une entrée n'existe que pendant la lecture.
static ACTIVE: Mutex<Vec<(u64, Arc<AtomicBool>)>> = Mutex::new(Vec::new());

fn active() -> MutexGuard<'static, Vec<(u64, Arc<AtomicBool>)>> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Nombre maximal de lectures simultanées ; au-delà, la requête reçoit `busy`.
const MAX_ACTIVE: usize = 6;

/// Admission d'une lecture : identifiant non utilisé et moins de `MAX_ACTIVE` lectures en cours.
fn admit(list: &mut Vec<(u64, Arc<AtomicBool>)>, id: u64) -> Result<Arc<AtomicBool>, &'static str> {
    if list.iter().any(|(i, _)| *i == id) {
        return Err("duplicate_id");
    }
    if list.len() >= MAX_ACTIVE {
        return Err("busy");
    }
    let flag = Arc::new(AtomicBool::new(false));
    list.push((id, flag.clone()));
    Ok(flag)
}

/// Enregistrement d'une lecture : retiré dès que la lecture se termine, même en cas d'erreur.
struct Registration {
    flag: Arc<AtomicBool>,
}

impl Registration {
    /// Admission et enregistrement sous un seul verrou : le compteur et l'unicité ne peuvent pas être dépassés en course.
    fn try_new(id: u64) -> Result<Self, &'static str> {
        let flag = admit(&mut active(), id)?;
        Ok(Registration { flag })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        active().retain(|(_, f)| !Arc::ptr_eq(f, &self.flag));
    }
}

/// Lance une lecture dans un fil dédié. L'admission et l'enregistrement de l'annulation se font avant le retour,
/// pour qu'une commande `native-range-cancel` envoyée juste après soit prise en compte. Un refus (`busy`,
/// `duplicate_id`) ou un fil non créé (`spawn_failed`) reçoit tout de suite une réponse finale.
pub fn start(req: NativeRangeRequest, reply: flume::Sender<String>) {
    let id = req.id;
    let registration = match Registration::try_new(id) {
        Ok(registration) => registration,
        Err(code) => {
            reply
                .send(RPCResponse::native_range_response(error_reply(id, code)))
                .ok();
            return;
        }
    };
    let fallback = reply.clone();
    let spawned = thread::Builder::new().spawn(move || {
        let mut sink =
            |payload: Value| reply.send(RPCResponse::native_range_chunk(payload)).is_ok();
        let payload = run(&req, &registration.flag, &mut sink);
        // Le créneau est libéré avant d'annoncer la fin : la page peut relancer une lecture dès la réponse reçue.
        drop(registration);
        reply.send(RPCResponse::native_range_response(payload)).ok();
    });
    if spawned.is_err() {
        // La fermeture non lancée est relâchée avec son enregistrement : l'identifiant redevient libre.
        fallback
            .send(RPCResponse::native_range_response(error_reply(
                id,
                "spawn_failed",
            )))
            .ok();
    }
}

/// Réponse finale pour une requête `native-range` illisible : l'identifiant est repris s'il est présent, sinon `null`.
pub fn bad_request(params: Option<&Value>) -> Value {
    let id = params
        .and_then(|p| p.get("id"))
        .and_then(Value::as_u64)
        .map_or(Value::Null, Value::from);
    json!({ "id": id, "error": "bad_request" })
}

/// Demande l'arrêt de la lecture `id` (sans effet si elle n'existe pas ou est terminée).
pub fn cancel(id: u64) {
    for (_, flag) in active().iter().filter(|(i, _)| *i == id) {
        flag.store(true, Ordering::SeqCst);
    }
}

/// Capacités publiées à la page (`native-range-caps`).
pub fn caps() -> Value {
    json!({ "version": VERSION, "maxChunk": MAX_CHUNK_BASE64, "maxRange": MAX_RANGE_BYTES })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(url: &str, range: Option<&str>, method: Option<&str>) -> NativeRangeRequest {
        let headers = range.map(|r| HashMap::from([("Range".to_owned(), r.to_owned())]));
        NativeRangeRequest {
            id: 5,
            url: url.into(),
            method: method.map(str::to_owned),
            headers,
            timeout_ms: None,
        }
    }

    fn head(status: u16, content_length: Option<u64>) -> ResponseHead {
        ResponseHead {
            status,
            content_range: Some("bytes 0-0/1".into()),
            content_length,
            headers: serde_json::Map::new(),
        }
    }

    #[test]
    fn hosts_reuse_the_debrid_allowlist() {
        for ok in [
            "https://example.debrid.it/f",
            "https://e1.example.alldebrid.com/f",
            "https://x.example.real-debrid.com/f",
            "https://api.torbox.app/f",
        ] {
            assert!(parse_allowed_url(ok).is_some(), "{}", ok);
        }
        for ko in [
            "http://example.debrid.it/f",
            "https://evil.com/f",
            "https://debrid.it/f",
            "https://example.debrid.it.evil.com/f",
            "https://user:pw@example.debrid.it/f",
            "https://example.debrid.it:8443/f",
            "https://127.0.0.1/f",
        ] {
            assert!(parse_allowed_url(ko).is_none(), "{}", ko);
        }
    }

    #[test]
    fn redirects_are_revalidated_per_hop() {
        assert!(redirect_allowed(
            &url::Url::parse("https://example.debrid.it/f2").unwrap()
        ));
        // Hôte nominatif public : permis (lien direct d'addon), la résolution publique est vérifiée à la connexion.
        assert!(redirect_allowed(
            &url::Url::parse("https://cdn.example.com/f2?token=a").unwrap()
        ));
        for ko in [
            "http://example.debrid.it/f2",
            "http://cdn.example.com/f2",
            "https://127.0.0.1/f2",
            "https://[::1]/f2",
            "https://192.168.1.2/f2",
            "https://localhost/f2",
            "https://u:p@cdn.example.com/f2",
            "https://cdn.example.com:8443/f2",
            "https://2130706433/f2",
        ] {
            assert!(!redirect_allowed(&url::Url::parse(ko).unwrap()), "{}", ko);
        }
    }

    fn resolves_to(ips: &'static [&'static str]) -> impl Fn(&str) -> Vec<IpAddr> {
        move |_: &str| ips.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn a_302_to_the_local_network_is_refused() {
        let base = url::Url::parse("https://files.example.tv/a.mp4").unwrap();
        for ko in [
            "http://files.example.tv/b.mp4",
            "http://cdn.example.com/b.mp4",
            "https://127.0.0.1/b.mp4",
            "https://127.1/b.mp4",
            "https://0x7f000001/b.mp4",
            "https://2130706433/b.mp4",
            "https://[::1]/b.mp4",
            "https://[::ffff:127.0.0.1]/b.mp4",
            "https://[fe80::1]/b.mp4",
            "https://10.0.0.5/b.mp4",
            "https://192.168.1.2/b.mp4",
            "https://169.254.169.254/latest/meta-data",
            "https://localhost/b.mp4",
            "https://localhost./b.mp4",
            "https://nas.local/b.mp4",
            "https://box.internal/b.mp4",
            "https://app.localhost/b.mp4",
            "https://monnas/b.mp4",
            "https://u:p@cdn.example.com/b.mp4",
            "https://cdn.example.com:8443/b.mp4",
            "https://cdn.example.com:80/b.mp4",
            "ftp://cdn.example.com/b.mp4",
            "file:///c:/windows/win.ini",
            "//127.0.0.1/b.mp4",
            "//cdn.example.com:8080/b.mp4",
        ] {
            assert_eq!(next_hop(&base, Some(ko), 0), None, "{}", ko);
        }
        // Sans `Location`, ou illisible : pas de saut.
        assert_eq!(next_hop(&base, None, 0), None);
        assert_eq!(next_hop(&base, Some("https://"), 0), None);
    }

    #[test]
    fn redirects_follow_valid_targets_up_to_the_hop_cap() {
        let base = url::Url::parse("https://files.example.tv/dir/a.mp4").unwrap();
        for (location, expected) in [
            ("/b.mp4?sig=1", "https://files.example.tv/b.mp4?sig=1"),
            ("c.mp4", "https://files.example.tv/dir/c.mp4"),
            (
                "https://cdn.example.com/b.mp4",
                "https://cdn.example.com/b.mp4",
            ),
            ("//cdn.example.com/b.mp4", "https://cdn.example.com/b.mp4"),
            (
                "https://e1.alldebrid.com/b.mp4",
                "https://e1.alldebrid.com/b.mp4",
            ),
        ] {
            assert_eq!(
                next_hop(&base, Some(location), 0).map(|u| u.to_string()),
                Some(expected.to_owned()),
                "{}",
                location
            );
        }
        // Au plus 3 redirections : la 4e réponse 3xx n'est pas suivie.
        assert!(next_hop(&base, Some("/b.mp4"), MAX_REDIRECTS - 1).is_some());
        assert_eq!(next_hop(&base, Some("/b.mp4"), MAX_REDIRECTS), None);
    }

    #[test]
    fn every_hop_is_resolved_and_must_be_public() {
        // Lien direct : épinglé sur les adresses publiques résolues, port 443.
        match plan_connection("cdn.example.com", &resolves_to(&["93.184.216.34"])) {
            Ok(Connection::Pinned(addrs)) => {
                assert_eq!(addrs, vec!["93.184.216.34:443".parse().unwrap()]);
            }
            other => panic!("{:?}", other),
        }
        // Rebinding : un nom d'apparence publique (y compris à la 2e redirection) qui se résout vers le réseau local.
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.0.10",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "fe80::1",
            "fd12::1",
            "::ffff:10.0.0.1",
        ] {
            let resolver = move |_: &str| vec![ip.parse::<IpAddr>().unwrap()];
            assert_eq!(
                plan_connection("rebind.example.com", &resolver),
                Err("host_not_allowed"),
                "{}",
                ip
            );
            // Un hôte de débrideur n'échappe pas à la règle.
            assert_eq!(
                plan_connection("e1.alldebrid.com", &resolver),
                Err("host_not_allowed"),
                "{}",
                ip
            );
        }
        assert_eq!(
            plan_connection(
                "rebind.example.com",
                &resolves_to(&["93.184.216.34", "10.0.0.1"])
            ),
            Err("host_not_allowed")
        );
        // Plage « fake-ip » des proxys TUN : refusée pour un lien direct, tolérée pour un débrideur ; adresse nulle (Pi-hole) : `network`.
        assert_eq!(
            plan_connection("cdn.example.com", &resolves_to(&["198.18.0.5"])),
            Err("host_not_allowed")
        );
        assert_eq!(
            plan_connection("e1.alldebrid.com", &resolves_to(&["198.18.0.5"])),
            Ok(Connection::Default)
        );
        assert_eq!(
            plan_connection("cdn.example.com", &resolves_to(&["0.0.0.0"])),
            Err("host_not_allowed")
        );
        assert_eq!(
            plan_connection("e1.alldebrid.com", &resolves_to(&["0.0.0.0"])),
            Err("network")
        );
        // Débrideur : résolution publique = connexion ordinaire (ni épinglage ni coupure du proxy).
        assert_eq!(
            plan_connection("e1.alldebrid.com", &resolves_to(&["93.184.216.34"])),
            Ok(Connection::Default)
        );
        // DNS indisponible : `network` pour un lien direct ; le débrideur échoue à la connexion.
        assert_eq!(
            plan_connection("cdn.example.com", &|_| Vec::new()),
            Err("network")
        );
        assert_eq!(
            plan_connection("e1.alldebrid.com", &|_| Vec::new()),
            Ok(Connection::Default)
        );
    }

    #[test]
    fn direct_addon_links_are_accepted_with_query_and_long_paths() {
        assert!(parse_source_url(
            "https://files.example.tv/dl/abc/Film.2024.1080p.mp4?sig=xyz&e=1"
        )
        .is_some());
        assert!(
            parse_source_url(&format!("https://files.example.tv/{}", "a".repeat(3000))).is_some()
        );
        assert!(
            parse_source_url(&format!("https://files.example.tv/{}", "a".repeat(5000))).is_none()
        );
        assert!(parse_source_url("https://x.real-debrid.com/d/ABC").is_some());
    }

    fn range_request(url: &str) -> NativeRangeRequest {
        request(url, Some("bytes=0-99"), None)
    }

    #[test]
    fn direct_link_resolving_to_a_private_address_is_refused_without_network() {
        let cancel = AtomicBool::new(false);
        let mut sink = |_: Value| true;
        for ip in [
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.20",
            "169.254.1.1",
            "::1",
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            let fin = run_with(
                &range_request("https://files.example.tv/a.mp4"),
                &cancel,
                &mut sink,
                &|_| vec![ip],
            );
            assert_eq!(fin["error"], "host_not_allowed", "{}", ip);
        }
        // Une seule adresse privée parmi des publiques suffit à refuser (rebinding).
        let mixed = |_: &str| {
            vec![
                "93.184.216.34".parse().unwrap(),
                "10.0.0.1".parse().unwrap(),
            ]
        };
        assert_eq!(
            run_with(
                &range_request("https://files.example.tv/a.mp4"),
                &cancel,
                &mut sink,
                &mixed
            )["error"],
            "host_not_allowed"
        );
        // Échec de résolution (DNS indisponible) : `network`, pas un refus.
        assert_eq!(
            run_with(
                &range_request("https://files.example.tv/a.mp4"),
                &cancel,
                &mut sink,
                &|_| vec![]
            )["error"],
            "network"
        );
    }

    #[test]
    fn unreadable_links_are_refused_before_any_resolution() {
        let cancel = AtomicBool::new(false);
        let mut sink = |_: Value| true;
        let boom = |_: &str| -> Vec<IpAddr> { panic!("pas de DNS pour un lien refusé") };
        for ko in [
            "http://files.example.tv/a.mp4",
            "https://127.0.0.1/a.mp4",
            "https://files.example.tv:8443/a",
            "magnet:?xt=urn:btih:abc",
            "ftp://files.example.tv/a",
        ] {
            assert_eq!(
                run_with(&range_request(ko), &cancel, &mut sink, &boom)["error"],
                "host_not_allowed",
                "{}",
                ko
            );
        }
    }

    #[test]
    fn range_header_is_strict_single_closed_range() {
        assert_eq!(
            parse_range("bytes=0-99"),
            Some(ByteRange { start: 0, end: 99 })
        );
        assert_eq!(parse_range("bytes=5-5").map(|r| r.size()), Some(1));
        for ko in [
            "bytes=99-0",
            "bytes=0-",
            "bytes=-100",
            "bytes=0-1,3-4",
            "bytes=a-b",
            "bytes= 0-1",
            "bytes=+1-2",
            "items=0-1",
            "bytes=0-1-2",
            "bytes=",
            "0-1",
        ] {
            assert_eq!(parse_range(ko), None, "{}", ko);
        }
        assert_eq!(parse_range("bytes=00000000000000000000001-2"), None);
    }

    #[test]
    fn range_header_is_required() {
        let mut headers = HashMap::new();
        assert_eq!(requested_range(None), Err("range_required"));
        assert_eq!(requested_range(Some(&headers)), Err("range_required"));
        headers.insert("range".to_owned(), "bytes=0-9".to_owned());
        assert_eq!(
            requested_range(Some(&headers)),
            Ok(ByteRange { start: 0, end: 9 })
        );
        headers.insert("range".to_owned(), "bytes=9-0".to_owned());
        assert_eq!(requested_range(Some(&headers)), Err("bad_range"));
    }

    #[test]
    fn partial_content_is_accepted() {
        let r = ByteRange {
            start: 100,
            end: 199,
        };
        assert_eq!(check_status(206, r, Some(100)), Ok(()));
        assert_eq!(check_status(206, r, None), Ok(()));
    }

    #[test]
    fn full_200_is_accepted_only_when_it_covers_the_content() {
        let whole = ByteRange { start: 0, end: 99 };
        assert_eq!(check_status(200, whole, Some(100)), Ok(()));
        assert_eq!(
            check_status(200, ByteRange { start: 0, end: 499 }, Some(100)),
            Ok(())
        );
        // Corps plus grand que la plage : le serveur a ignoré Range sur un fichier entier.
        assert_eq!(
            check_status(200, ByteRange { start: 0, end: 49 }, Some(100)),
            Err("range_unsupported")
        );
        // Plage décalée : un 200 renvoie toujours le fichier depuis 0, donc jamais la plage demandée.
        assert_eq!(
            check_status(200, ByteRange { start: 5, end: 20 }, Some(10)),
            Err("range_unsupported")
        );
        assert_eq!(check_status(200, whole, None), Err("range_unsupported"));
    }

    #[test]
    fn other_statuses_are_reported_not_served() {
        let r = ByteRange { start: 0, end: 9 };
        assert_eq!(check_status(416, r, None), Err("bad_status"));
        assert_eq!(check_status(403, r, None), Err("bad_status"));
    }

    #[test]
    fn body_is_chunked_within_one_mebibyte_of_base64() {
        let body: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8).collect();
        let range = ByteRange {
            start: 0,
            end: body.len() as u64 - 1,
        };
        let mut out = Vec::new();
        let mut sink = |v: Value| {
            out.push(v);
            true
        };
        let cancel = AtomicBool::new(false);
        let mut h = head(206, Some(body.len() as u64));
        h.content_range = Some("bytes 0-1999999/2000000".into());
        let fin = deliver(9, range, h, &body[..], &cancel, &mut sink);
        assert_eq!(fin["status"], 206);
        assert_eq!(fin["contentLength"], 2_000_000);
        assert_eq!(fin["chunks"], 3);
        assert_eq!(fin["contentRange"], "bytes 0-1999999/2000000");
        assert_eq!(out.len(), 3);
        let mut joined = Vec::new();
        for (i, c) in out.iter().enumerate() {
            assert_eq!(c["id"], 9);
            assert_eq!(c["seq"], i as u64);
            assert_eq!(c["last"], false);
            let b64 = c["bodyBase64"].as_str().unwrap();
            assert!(
                b64.len() <= MAX_CHUNK_BASE64,
                "chunk {} is {} chars",
                i,
                b64.len()
            );
            joined.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .unwrap(),
            );
        }
        assert_eq!(joined, body);
    }

    #[test]
    fn empty_body_emits_no_chunk() {
        let mut out = Vec::new();
        let mut sink = |v: Value| {
            out.push(v);
            true
        };
        let cancel = AtomicBool::new(false);
        let fin = deliver(
            3,
            ByteRange { start: 0, end: 9 },
            head(206, Some(0)),
            &[0u8; 0][..],
            &cancel,
            &mut sink,
        );
        assert_eq!(fin["chunks"], 0);
        assert_eq!(fin["contentLength"], 0);
        assert!(out.is_empty());
    }

    #[test]
    fn oversized_body_is_refused() {
        let cancel = AtomicBool::new(false);
        let mut sink = |_: Value| true;
        let body = [1u8; 11];
        let fin = deliver(
            3,
            ByteRange { start: 0, end: 9 },
            head(206, None),
            &body[..],
            &cancel,
            &mut sink,
        );
        assert_eq!(fin["error"], "too_large");
        // Content-Length annoncé trop grand : refus avant toute lecture.
        let fin = deliver(
            3,
            ByteRange { start: 0, end: 9 },
            head(206, Some(11)),
            &body[..],
            &cancel,
            &mut sink,
        );
        assert_eq!(fin["error"], "too_large");
        assert_eq!(fin["status"], 206);
    }

    #[test]
    fn cancel_before_start_emits_nothing() {
        let cancel = AtomicBool::new(true);
        let mut out = Vec::new();
        let mut sink = |v: Value| {
            out.push(v);
            true
        };
        let fin = deliver(
            4,
            ByteRange { start: 0, end: 9 },
            head(206, Some(10)),
            &[1u8; 10][..],
            &cancel,
            &mut sink,
        );
        assert_eq!(fin["error"], "cancelled");
        assert!(out.is_empty());
    }

    #[test]
    fn cancel_stops_emission_midway() {
        let cancel = AtomicBool::new(false);
        let body = vec![7u8; 3 * CHUNK_RAW_BYTES];
        let mut out = Vec::new();
        let mut sink = |v: Value| {
            out.push(v);
            cancel.store(true, Ordering::SeqCst);
            true
        };
        let range = ByteRange {
            start: 0,
            end: body.len() as u64 - 1,
        };
        let fin = deliver(
            4,
            range,
            head(206, Some(body.len() as u64)),
            &body[..],
            &cancel,
            &mut sink,
        );
        assert_eq!(fin["error"], "cancelled");
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn oversized_range_is_refused_before_any_network_use() {
        let r = request("https://example.debrid.it/f", Some("bytes=0-8388608"), None);
        let cancel = AtomicBool::new(false);
        let mut sink = |_: Value| true;
        assert_eq!(run(&r, &cancel, &mut sink)["error"], "too_large");
    }

    #[test]
    fn invalid_requests_are_refused_without_network() {
        let cancel = AtomicBool::new(false);
        let mut sink = |_: Value| true;
        let check = |r: NativeRangeRequest, sink: &mut dyn FnMut(Value) -> bool| {
            run(&r, &cancel, sink)["error"].clone()
        };
        assert_eq!(
            check(
                request(
                    "https://example.debrid.it/f",
                    Some("bytes=0-9"),
                    Some("POST")
                ),
                &mut sink
            ),
            "method_not_allowed"
        );
        assert_eq!(
            check(
                request("https://127.0.0.1/f", Some("bytes=0-9"), None),
                &mut sink
            ),
            "host_not_allowed"
        );
        assert_eq!(
            check(
                request("https://example.debrid.it/f", None, None),
                &mut sink
            ),
            "range_required"
        );
        assert_eq!(
            check(
                request("https://example.debrid.it/f", Some("bytes=0-"), None),
                &mut sink
            ),
            "bad_range"
        );
    }

    #[test]
    fn registration_is_cancelable_then_released() {
        let id = 9_100_001;
        let reg = Registration::try_new(id).unwrap();
        assert!(!reg.flag.load(Ordering::SeqCst));
        cancel(id);
        assert!(reg.flag.load(Ordering::SeqCst));
        drop(reg);
        assert!(active().iter().all(|(i, _)| *i != id));
        cancel(id);
    }

    #[test]
    fn admission_refuses_duplicate_ids_and_caps_active_reads() {
        let mut list = Vec::new();
        for id in 1..=MAX_ACTIVE as u64 {
            assert!(admit(&mut list, id).is_ok());
        }
        assert_eq!(admit(&mut list, 99).err(), Some("busy"));
        // Un identifiant déjà actif est refusé même quand la limite est atteinte.
        assert_eq!(admit(&mut list, 1).err(), Some("duplicate_id"));
        list.pop();
        assert!(admit(&mut list, 99).is_ok());
        assert_eq!(admit(&mut list, 99).err(), Some("duplicate_id"));
        assert_eq!(admit(&mut list, 100).err(), Some("busy"));
    }

    #[test]
    fn unreadable_requests_get_a_final_bad_request_reply() {
        assert_eq!(
            bad_request(Some(&json!({ "id": 7, "url": 42 }))),
            json!({ "id": 7, "error": "bad_request" })
        );
        assert_eq!(
            bad_request(Some(&json!({ "id": "7" }))),
            json!({ "id": null, "error": "bad_request" })
        );
        assert_eq!(
            bad_request(Some(&json!({ "url": "https://example.debrid.it/f" }))),
            json!({ "id": null, "error": "bad_request" })
        );
        assert_eq!(
            bad_request(None),
            json!({ "id": null, "error": "bad_request" })
        );
    }

    #[test]
    fn caps_and_wire_messages_follow_the_protocol() {
        assert_eq!(
            caps(),
            json!({ "version": 1, "maxChunk": 1_048_576, "maxRange": 8_388_608 })
        );
        let chunk: Value =
            serde_json::from_str(&RPCResponse::native_range_chunk(json!({ "id": 1 }))).unwrap();
        assert_eq!(chunk["args"][0], "native-range-chunk");
        assert_eq!(chunk["args"][1]["id"], 1);
        let fin: Value = serde_json::from_str(&RPCResponse::native_range_response(
            json!({ "id": 1, "error": "x" }),
        ))
        .unwrap();
        assert_eq!(fin["args"][0], "native-range-response");
        assert_eq!(fin["args"][1]["error"], "x");
    }
}
