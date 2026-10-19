//! Pont de lecture par plages « native-range » : lit depuis cet appareil un fichier débridé par tranches `Range`, sans tout charger d'un coup.
//! Même allowlist d'hôtes que `native-http` (débrideurs et leurs CDN, HTTPS seulement, voir native_http.rs). GET uniquement, en-tête
//! `Range: bytes=a-b` obligatoire, plage <= 8 Mo, 3 redirections au plus (chaque saut revalidé). Un 200 n'est accepté que si le corps
//! complet, depuis l'octet 0, tient dans la plage demandée ; sinon `range_unsupported`.
//!
//! Protocole (signaux vers la page, ids de requête fournis par la page) :
//! - requête `native-range` : `{id, url, headers: {range}, method?, timeoutMs?}` ;
//! - `native-range-chunk` : `{id, seq, bodyBase64, last: false}` (seq à partir de 0), émis pendant la lecture, morceaux <= 1 Mo en base64 ;
//! - `native-range-response` (toujours en dernier) : `{id, status, contentRange, contentLength, headers, chunks}` ou `{id, error, status?}` ;
//! - annulation `native-range-cancel` : `{id}` ; la lecture s'arrête et la réponse finale est `{id, error: "cancelled"}` ;
//! - capacités `native-range-caps` : répond `native-range-caps` avec `{version: 1, maxChunk, maxRange}`.
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

use crate::stremio_app::ipc::RPCResponse;
use crate::stremio_app::native_http::{parse_allowed_url, RESPONSE_HEADERS};

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
        .and_then(|h| h.iter().find(|(k, _)| k.eq_ignore_ascii_case("range")).map(|(_, v)| v.as_str()))
        .ok_or("range_required")?;
    parse_range(raw).ok_or("bad_range")
}

/// Une redirection n'est suivie que vers une URL autorisée : mêmes règles que la requête initiale.
pub fn redirect_allowed(url: &url::Url) -> bool {
    parse_allowed_url(url.as_str()).is_some()
}

/// Décide si la réponse peut servir la plage. 206 : oui. 200 : seulement si le corps complet (depuis l'octet 0) tient dans la plage.
pub fn check_status(status: u16, range: ByteRange, content_length: Option<u64>) -> Result<(), &'static str> {
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
fn emit(id: u64, seq: u64, bytes: &[u8], cancel: &AtomicBool, sink: &mut dyn FnMut(Value) -> bool) -> bool {
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
    if head.content_length.map_or(false, |n| n > limit) {
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
pub fn run(req: &NativeRangeRequest, cancel: &AtomicBool, sink: &mut dyn FnMut(Value) -> bool) -> Value {
    let id = req.id;
    if !req.method.as_deref().unwrap_or("GET").eq_ignore_ascii_case("GET") {
        return error_reply(id, "method_not_allowed");
    }
    let url = match parse_allowed_url(&req.url) {
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
    let timeout = Duration::from_millis(req.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS));
    // Redirections suivies seulement vers un hôte autorisé (3 au plus), comme native-http.
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS || !redirect_allowed(attempt.url()) {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });
    let client = match reqwest::blocking::Client::builder().timeout(timeout).redirect(policy).build() {
        Ok(c) => c,
        Err(_) => return error_reply(id, "client"),
    };
    let response = match client
        .get(url)
        .header(reqwest::header::RANGE, format!("bytes={}-{}", range.start, range.end))
        .send()
    {
        Ok(r) => r,
        Err(e) if e.is_timeout() => return error_reply(id, "timeout"),
        Err(_) => return error_reply(id, "network"),
    };
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return error_with_status(id, "redirect_not_allowed", status);
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
    let head = ResponseHead { status, content_range, content_length, headers };
    deliver(id, range, head, response, cancel, sink)
}

/// Annulations en cours, par identifiant de requête. Une entrée n'existe que pendant la lecture.
static ACTIVE: Mutex<Vec<(u64, Arc<AtomicBool>)>> = Mutex::new(Vec::new());

fn active() -> MutexGuard<'static, Vec<(u64, Arc<AtomicBool>)>> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Enregistrement d'une lecture : retiré dès que la lecture se termine, même en cas d'erreur.
struct Registration {
    flag: Arc<AtomicBool>,
}

impl Registration {
    fn new(id: u64) -> Self {
        let flag = Arc::new(AtomicBool::new(false));
        active().push((id, flag.clone()));
        Registration { flag }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        active().retain(|(_, f)| !Arc::ptr_eq(f, &self.flag));
    }
}

/// Lance une lecture dans un fil dédié. L'enregistrement de l'annulation se fait avant le retour,
/// pour qu'une commande `native-range-cancel` envoyée juste après soit prise en compte.
pub fn start(req: NativeRangeRequest, reply: flume::Sender<String>) {
    let registration = Registration::new(req.id);
    thread::spawn(move || {
        let mut sink = |payload: Value| reply.send(RPCResponse::native_range_chunk(payload)).is_ok();
        let payload = run(&req, &registration.flag, &mut sink);
        reply.send(RPCResponse::native_range_response(payload)).ok();
    });
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
        NativeRangeRequest { id: 5, url: url.into(), method: method.map(str::to_owned), headers, timeout_ms: None }
    }

    fn head(status: u16, content_length: Option<u64>) -> ResponseHead {
        ResponseHead { status, content_range: Some("bytes 0-0/1".into()), content_length, headers: serde_json::Map::new() }
    }

    #[test]
    fn hosts_reuse_the_debrid_allowlist() {
        for ok in ["https://example.debrid.it/f", "https://e1.example.alldebrid.com/f", "https://x.example.real-debrid.com/f", "https://api.torbox.app/f"] {
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
        assert!(redirect_allowed(&url::Url::parse("https://example.debrid.it/f2").unwrap()));
        assert!(!redirect_allowed(&url::Url::parse("https://evil.com/f2").unwrap()));
        assert!(!redirect_allowed(&url::Url::parse("http://example.debrid.it/f2").unwrap()));
    }

    #[test]
    fn range_header_is_strict_single_closed_range() {
        assert_eq!(parse_range("bytes=0-99"), Some(ByteRange { start: 0, end: 99 }));
        assert_eq!(parse_range("bytes=5-5").map(|r| r.size()), Some(1));
        for ko in ["bytes=99-0", "bytes=0-", "bytes=-100", "bytes=0-1,3-4", "bytes=a-b", "bytes= 0-1", "bytes=+1-2", "items=0-1", "bytes=0-1-2", "bytes=", "0-1"] {
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
        assert_eq!(requested_range(Some(&headers)), Ok(ByteRange { start: 0, end: 9 }));
        headers.insert("range".to_owned(), "bytes=9-0".to_owned());
        assert_eq!(requested_range(Some(&headers)), Err("bad_range"));
    }

    #[test]
    fn partial_content_is_accepted() {
        let r = ByteRange { start: 100, end: 199 };
        assert_eq!(check_status(206, r, Some(100)), Ok(()));
        assert_eq!(check_status(206, r, None), Ok(()));
    }

    #[test]
    fn full_200_is_accepted_only_when_it_covers_the_content() {
        let whole = ByteRange { start: 0, end: 99 };
        assert_eq!(check_status(200, whole, Some(100)), Ok(()));
        assert_eq!(check_status(200, ByteRange { start: 0, end: 499 }, Some(100)), Ok(()));
        // Corps plus grand que la plage : le serveur a ignoré Range sur un fichier entier.
        assert_eq!(check_status(200, ByteRange { start: 0, end: 49 }, Some(100)), Err("range_unsupported"));
        // Plage décalée : un 200 renvoie toujours le fichier depuis 0, donc jamais la plage demandée.
        assert_eq!(check_status(200, ByteRange { start: 5, end: 20 }, Some(10)), Err("range_unsupported"));
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
        let range = ByteRange { start: 0, end: body.len() as u64 - 1 };
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
            assert!(b64.len() <= MAX_CHUNK_BASE64, "chunk {} is {} chars", i, b64.len());
            joined.extend(base64::engine::general_purpose::STANDARD.decode(b64).unwrap());
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
        let fin = deliver(3, ByteRange { start: 0, end: 9 }, head(206, Some(0)), &[0u8; 0][..], &cancel, &mut sink);
        assert_eq!(fin["chunks"], 0);
        assert_eq!(fin["contentLength"], 0);
        assert!(out.is_empty());
    }

    #[test]
    fn oversized_body_is_refused() {
        let cancel = AtomicBool::new(false);
        let mut sink = |_: Value| true;
        let body = [1u8; 11];
        let fin = deliver(3, ByteRange { start: 0, end: 9 }, head(206, None), &body[..], &cancel, &mut sink);
        assert_eq!(fin["error"], "too_large");
        // Content-Length annoncé trop grand : refus avant toute lecture.
        let fin = deliver(3, ByteRange { start: 0, end: 9 }, head(206, Some(11)), &body[..], &cancel, &mut sink);
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
        let fin = deliver(4, ByteRange { start: 0, end: 9 }, head(206, Some(10)), &[1u8; 10][..], &cancel, &mut sink);
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
        let range = ByteRange { start: 0, end: body.len() as u64 - 1 };
        let fin = deliver(4, range, head(206, Some(body.len() as u64)), &body[..], &cancel, &mut sink);
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
        let check = |r: NativeRangeRequest, sink: &mut dyn FnMut(Value) -> bool| run(&r, &cancel, sink)["error"].clone();
        assert_eq!(check(request("https://example.debrid.it/f", Some("bytes=0-9"), Some("POST")), &mut sink), "method_not_allowed");
        assert_eq!(check(request("https://evil.com/f", Some("bytes=0-9"), None), &mut sink), "host_not_allowed");
        assert_eq!(check(request("https://example.debrid.it/f", None, None), &mut sink), "range_required");
        assert_eq!(check(request("https://example.debrid.it/f", Some("bytes=0-"), None), &mut sink), "bad_range");
    }

    #[test]
    fn registration_is_cancelable_then_released() {
        let id = 9_100_001;
        let reg = Registration::new(id);
        assert!(!reg.flag.load(Ordering::SeqCst));
        cancel(id);
        assert!(reg.flag.load(Ordering::SeqCst));
        drop(reg);
        assert!(active().iter().all(|(i, _)| *i != id));
        cancel(id);
    }

    #[test]
    fn caps_and_wire_messages_follow_the_protocol() {
        assert_eq!(caps(), json!({ "version": 1, "maxChunk": 1_048_576, "maxRange": 8_388_608 }));
        let chunk: Value = serde_json::from_str(&RPCResponse::native_range_chunk(json!({ "id": 1 }))).unwrap();
        assert_eq!(chunk["args"][0], "native-range-chunk");
        assert_eq!(chunk["args"][1]["id"], 1);
        let fin: Value = serde_json::from_str(&RPCResponse::native_range_response(json!({ "id": 1, "error": "x" }))).unwrap();
        assert_eq!(fin["args"][0], "native-range-response");
        assert_eq!(fin["args"][1]["error"], "x");
    }
}
