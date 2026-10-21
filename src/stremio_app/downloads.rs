//! Téléchargements de la coque (ODY-201) : E/S fichier du pont `download-*`, dans le dossier choisi par l'utilisateur
//! (par défaut `<Vidéos>\Laststream`). Aucune requête réseau ici : la page télécharge (débrideurs, côté appareil) et envoie
//! les octets par morceaux base64 ; la coque écrit, tronque, finalise, indique la taille, localise, supprime et mesure.
//!
//! Protocole : requête `{id, args: [méthode, params]}`, réponse `["méthode", payload]` où le payload vaut `{ok: true, ...}`
//! ou `{ok: false, code, message}`. Codes : `bad_id`, `bad_range`, `too_large`, `not_found`, `store_full`, `io`, `no_folder`,
//! et `bad_data` (base64 illisible, hors liste de la spec).
//!
//! - `id` : uniquement `[A-Za-z0-9._-]`, 1 à 128 caractères, ni `.` ni `..`, sans nom de périphérique Windows (`CON`, `NUL`,
//!   `COM1`…). Les chemins sont toujours `dossier + id + suffixe` : aucun chemin ne vient de la page.
//! - Les méthodes passent une à une, dans l'ordre d'arrivée, sur un seul fil de travail : pas de course entre `write` et `finalize`.
//! - `download-folder-pick` n'est pas ici : le sélecteur tourne sur le fil de l'interface (app.rs) et `finish_pick` persiste le choix.
//!
//! Rien n'est journalisé (ni identifiant, ni contenu, ni chemin).

use base64::Engine;
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
    thread,
};
use winapi::{shared::ntdef::ULARGE_INTEGER, um::fileapi::GetDiskFreeSpaceExW};

use crate::stremio_app::{ipc::RPCResponse, shell_config};

/// Taille maximale des données d'une écriture, une fois décodées.
pub const MAX_WRITE_BYTES: usize = 16 * 1024 * 1024;
/// Longueur maximale de la chaîne base64 d'une écriture : le codage de 16 Mio.
const MAX_WRITE_BASE64: usize = MAX_WRITE_BYTES.div_ceil(3) * 4;
const MAX_ID_LEN: usize = 128;
const PART_SUFFIX: &str = "part";
/// Extensions des fichiers finaux, dans l'ordre de recherche.
const FINAL_EXTENSIONS: [&str; 3] = ["mkv", "mp4", "bin"];
const FOLDER_NAME: &str = "Laststream";
/// Méthodes exécutées sur le fil de travail (`download-folder-pick` a son propre chemin, sur le fil de l'interface).
const WORKER_METHODS: [&str; 9] = [
    "download-folder-get",
    "download-open",
    "download-write",
    "download-truncate",
    "download-size",
    "download-finalize",
    "download-path",
    "download-remove",
    "download-usage",
];
/// Codes Windows d'un disque plein (`ERROR_HANDLE_DISK_FULL`, `ERROR_DISK_FULL`).
const ERROR_HANDLE_DISK_FULL: i32 = 39;
const ERROR_DISK_FULL: i32 = 112;

/// Échec d'une méthode : code de la spec et message court (anglais, pour le journal de la page).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DlError {
    pub code: &'static str,
    pub message: &'static str,
}

const BAD_ID: DlError = DlError {
    code: "bad_id",
    message: "invalid download id",
};
const BAD_RANGE: DlError = DlError {
    code: "bad_range",
    message: "offset or length out of range",
};
const TOO_LARGE: DlError = DlError {
    code: "too_large",
    message: "data larger than 16 MiB",
};
const BAD_DATA: DlError = DlError {
    code: "bad_data",
    message: "data is not valid base64",
};
const NOT_FOUND: DlError = DlError {
    code: "not_found",
    message: "no such download",
};
const ALREADY_FINAL: DlError = DlError {
    code: "not_found",
    message: "download already finalized",
};
const STORE_FULL: DlError = DlError {
    code: "store_full",
    message: "not enough disk space",
};
const IO: DlError = DlError {
    code: "io",
    message: "file operation failed",
};
const NO_FOLDER: DlError = DlError {
    code: "no_folder",
    message: "downloads folder unavailable",
};

/// Validation stricte de l'identifiant : seul chemin par lequel la page désigne un fichier.
pub fn validate_id(id: &str) -> Result<&str, DlError> {
    let charset = id
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'));
    let valid = (1..=MAX_ID_LEN).contains(&id.len())
        && charset
        && id != "."
        && id != ".."
        && !is_device_name(id);
    if valid {
        Ok(id)
    } else {
        Err(BAD_ID)
    }
}

/// Noms de périphériques réservés de Windows : `CON.part` désigne la console, même avec une extension.
fn is_device_name(id: &str) -> bool {
    let stem = id.split('.').next().unwrap_or_default();
    let fixed = ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|name| stem.eq_ignore_ascii_case(name));
    let bytes = stem.as_bytes();
    let numbered = bytes.len() == 4
        && (bytes[..3].eq_ignore_ascii_case(b"COM") || bytes[..3].eq_ignore_ascii_case(b"LPT"))
        && (b'1'..=b'9').contains(&bytes[3]);
    fixed || numbered
}

fn part_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.{PART_SUFFIX}"))
}

fn final_path(dir: &Path, id: &str, ext: &str) -> PathBuf {
    dir.join(format!("{id}.{ext}"))
}

/// Fichier final de `id`, quelle que soit son extension.
fn find_final(dir: &Path, id: &str) -> Option<PathBuf> {
    FINAL_EXTENSIONS
        .iter()
        .map(|ext| final_path(dir, id, ext))
        .find(|path| path.is_file())
}

/// Extension d'après les premiers octets : EBML (Matroska) → `mkv`, `ftyp` en position 4 (MP4/MOV) → `mp4`, sinon `bin`.
pub fn detect_extension(head: &[u8]) -> &'static str {
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        "mkv"
    } else if head.len() >= 8 && &head[4..8] == b"ftyp" {
        "mp4"
    } else {
        "bin"
    }
}

/// Dossier par défaut : `<Vidéos>\Laststream`, repli sur `<Profil>\Videos\Laststream`.
pub fn default_download_dir(videos: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    videos
        .or_else(|| home.map(|home| home.join("Videos")))
        .map(|dir| dir.join(FOLDER_NAME))
}

/// Dossier à utiliser : le dossier configuré s'il est absolu, sinon le dossier par défaut. Créé au besoin ;
/// `no_folder` s'il est impossible à créer. Renvoie aussi `isDefault`.
pub fn resolve_folder(
    configured: Option<&str>,
    default: Option<PathBuf>,
) -> Result<(PathBuf, bool), DlError> {
    let (dir, is_default) = match configured.map(Path::new).filter(|path| path.is_absolute()) {
        Some(path) => (path.to_path_buf(), false),
        None => (default.ok_or(NO_FOLDER)?, true),
    };
    fs::create_dir_all(&dir).map_err(|_| NO_FOLDER)?;
    Ok((dir, is_default))
}

fn current_folder() -> Result<(PathBuf, bool), DlError> {
    resolve_folder(
        shell_config::downloads_dir().as_deref(),
        default_download_dir(dirs::video_dir(), dirs::home_dir()),
    )
}

fn path_text(path: &Path) -> Result<String, DlError> {
    path.to_str().map(str::to_owned).ok_or(IO)
}

/// Traduit une erreur d'E/S : disque plein → `store_full`, fichier absent → `not_found`, sinon `io`.
fn map_io(error: io::Error) -> DlError {
    match error.raw_os_error() {
        Some(ERROR_DISK_FULL) | Some(ERROR_HANDLE_DISK_FULL) => STORE_FULL,
        _ if error.kind() == io::ErrorKind::NotFound => NOT_FOUND,
        _ => IO,
    }
}

fn remove_if_exists(path: &Path) -> Result<(), DlError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(map_io(error)),
    }
}

/// Taille courante : celle du `.part`, sinon celle du fichier final, sinon 0.
fn current_size(dir: &Path, id: &str) -> Result<u64, DlError> {
    let part = part_path(dir, id);
    let path = if part.is_file() {
        part
    } else {
        match find_final(dir, id) {
            Some(path) => path,
            None => return Ok(0),
        }
    };
    Ok(fs::metadata(&path).map_err(map_io)?.len())
}

/// Ouvre (ou reprend) un téléchargement : crée le `.part` vide au besoin. Si le fichier final existe déjà, le dit.
pub fn open(dir: &Path, id: &str) -> Result<Value, DlError> {
    validate_id(id)?;
    if let Some(path) = find_final(dir, id) {
        let size = fs::metadata(&path).map_err(map_io)?.len();
        return Ok(json!({ "ok": true, "size": size, "finalized": true }));
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(part_path(dir, id))
        .map_err(map_io)?;
    let size = file.metadata().map_err(map_io)?.len();
    Ok(json!({ "ok": true, "size": size }))
}

/// Écrit `data_base64` à `offset` dans le `.part`. `offset` ne peut pas dépasser la taille courante ; les données
/// décodées tiennent dans 16 Mio. La page n'a jamais le fichier entier en mémoire : une écriture = un morceau.
pub fn write_chunk(dir: &Path, id: &str, offset: u64, data_base64: &str) -> Result<Value, DlError> {
    validate_id(id)?;
    if data_base64.len() > MAX_WRITE_BASE64 {
        return Err(TOO_LARGE);
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(data_base64)
        .map_err(|_| BAD_DATA)?;
    if data.len() > MAX_WRITE_BYTES {
        return Err(TOO_LARGE);
    }
    let part = part_path(dir, id);
    if !part.is_file() && find_final(dir, id).is_some() {
        return Err(ALREADY_FINAL);
    }
    if offset > current_size(dir, id)? {
        return Err(BAD_RANGE);
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&part)
        .map_err(map_io)?;
    file.seek(SeekFrom::Start(offset)).map_err(map_io)?;
    file.write_all(&data).map_err(map_io)?;
    let size = file.metadata().map_err(map_io)?.len();
    Ok(json!({ "ok": true, "size": size }))
}

/// Tronque le `.part` à `len` octets ; `len` ne peut pas dépasser la taille courante.
pub fn truncate(dir: &Path, id: &str, len: u64) -> Result<Value, DlError> {
    validate_id(id)?;
    let part = part_path(dir, id);
    if !part.is_file() {
        if find_final(dir, id).is_some() {
            return Err(ALREADY_FINAL);
        }
        // Aucun fichier : la taille est 0, seule une troncature à 0 a un sens.
        return if len == 0 {
            Ok(json!({ "ok": true, "size": 0 }))
        } else {
            Err(BAD_RANGE)
        };
    }
    let file = OpenOptions::new().write(true).open(&part).map_err(map_io)?;
    let size = file.metadata().map_err(map_io)?.len();
    if len > size {
        return Err(BAD_RANGE);
    }
    file.set_len(len).map_err(map_io)?;
    Ok(json!({ "ok": true, "size": len }))
}

/// Taille du téléchargement (`.part`, sinon fichier final, sinon 0).
pub fn size(dir: &Path, id: &str) -> Result<Value, DlError> {
    validate_id(id)?;
    Ok(json!({ "ok": true, "size": current_size(dir, id)? }))
}

/// Finalise : sync des données, puis renomme `<id>.part` en `<id>.<ext>` (ext devinée par signature). Un appel répété
/// renvoie le fichier final déjà en place.
pub fn finalize(dir: &Path, id: &str) -> Result<Value, DlError> {
    validate_id(id)?;
    let part = part_path(dir, id);
    if !part.is_file() {
        let path = find_final(dir, id).ok_or(NOT_FOUND)?;
        let size = fs::metadata(&path).map_err(map_io)?.len();
        return Ok(json!({ "ok": true, "path": path_text(&path)?, "size": size }));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&part)
        .map_err(map_io)?;
    let size = file.metadata().map_err(map_io)?.len();
    let head_len = size.min(8) as usize;
    let mut head = [0u8; 8];
    file.read_exact(&mut head[..head_len]).map_err(map_io)?;
    file.sync_data().map_err(map_io)?;
    drop(file);
    let ext = detect_extension(&head[..head_len]);
    let target = final_path(dir, id, ext);
    fs::rename(&part, &target).map_err(map_io)?;
    // Une version finale d'une autre extension ne doit pas rester à côté du nouveau fichier.
    for other in FINAL_EXTENSIONS.iter().filter(|other| **other != ext) {
        remove_if_exists(&final_path(dir, id, other))?;
    }
    Ok(json!({ "ok": true, "path": path_text(&target)?, "size": size }))
}

/// Chemin du fichier final (pour `loadfile` de mpv), ou `not_found`.
pub fn locate(dir: &Path, id: &str) -> Result<Value, DlError> {
    validate_id(id)?;
    let path = find_final(dir, id).ok_or(NOT_FOUND)?;
    Ok(json!({ "ok": true, "path": path_text(&path)? }))
}

/// Supprime le `.part` et le fichier final. Idempotent.
pub fn remove(dir: &Path, id: &str) -> Result<Value, DlError> {
    validate_id(id)?;
    remove_if_exists(&part_path(dir, id))?;
    for ext in FINAL_EXTENSIONS {
        remove_if_exists(&final_path(dir, id, ext))?;
    }
    Ok(json!({ "ok": true }))
}

/// `used` : somme des fichiers du dossier ; `available` : espace libre du volume.
pub fn usage(dir: &Path) -> Result<Value, DlError> {
    let mut used = 0u64;
    for entry in fs::read_dir(dir).map_err(map_io)? {
        let entry = entry.map_err(map_io)?;
        if let Ok(metadata) = entry.metadata() {
            if metadata.is_file() {
                used = used.saturating_add(metadata.len());
            }
        }
    }
    Ok(json!({ "ok": true, "used": used, "available": available_space(dir)? }))
}

/// Espace libre du volume qui contient `dir`, en octets.
fn available_space(dir: &Path) -> Result<u64, DlError> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = dir
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: un ULARGE_INTEGER est un entier de 64 bits sans invariant ; tout motif binaire est valide.
    let mut available: ULARGE_INTEGER = unsafe { std::mem::zeroed() };
    // SAFETY: `wide` est une chaîne UTF-16 terminée par zéro, vivante pendant l'appel ; `available` est un emplacement
    // valide ; les deux autres sorties sont facultatives (pointeurs nuls).
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(map_io(io::Error::last_os_error()));
    }
    // SAFETY: GetDiskFreeSpaceExW vient d'écrire la variante `QuadPart`.
    Ok(unsafe { *available.QuadPart() })
}

/// Charge `{ok, ...}` ou `{ok: false, code, message}`.
fn envelope(outcome: Result<Value, DlError>) -> Value {
    match outcome {
        Ok(value) => value,
        Err(error) => json!({ "ok": false, "code": error.code, "message": error.message }),
    }
}

/// Dossier courant et `isDefault`, pour `download-folder-get`.
fn folder_get() -> Result<Value, DlError> {
    let (dir, is_default) = current_folder()?;
    Ok(json!({ "ok": true, "path": path_text(&dir)?, "isDefault": is_default }))
}

/// Suite du sélecteur de dossier (fil de l'interface) : `selected` vaut le dossier choisi, `None` si annulé, ou une erreur
/// si le chemin n'est pas lisible. Le choix est écrit dans `shell.json` (`downloads_dir`), les autres clés sont conservées.
pub fn finish_pick(selected: Result<Option<String>, String>) -> Value {
    let outcome = match selected {
        Ok(Some(path)) => shell_config::save_downloads_dir(&path)
            .map(|()| json!({ "ok": true, "path": path }))
            .map_err(|_| IO),
        Ok(None) => Ok(json!({ "ok": true, "path": null })),
        Err(_) => Err(IO),
    };
    envelope(outcome)
}

/// Méthode `download-*` traitée par le fil de travail.
pub fn is_worker_method(method: &str) -> bool {
    WORKER_METHODS.contains(&method)
}

/// Une méthode en attente, et le canal de la page pour sa réponse.
struct Job {
    method: String,
    params: Value,
    reply: flume::Sender<String>,
}

static WORKER: OnceLock<flume::Sender<Job>> = OnceLock::new();

/// Met une méthode `download-*` en file ; sa réponse part sur `reply` une fois exécutée. Une seule file, dans l'ordre.
pub fn submit(method: &str, params: Option<&Value>, reply: flume::Sender<String>) {
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = flume::unbounded::<Job>();
        thread::spawn(move || {
            for job in rx {
                let payload = execute(&job.method, &job.params);
                job.reply
                    .send(RPCResponse::download_reply(&job.method, payload))
                    .ok();
            }
        });
        tx
    });
    worker
        .send(Job {
            method: method.to_owned(),
            params: params.cloned().unwrap_or(Value::Null),
            reply,
        })
        .ok();
}

/// Exécute une méthode et renvoie sa charge. Le dossier est résolu à chaque appel (le réglage peut changer).
pub fn execute(method: &str, params: &Value) -> Value {
    let outcome = match method {
        "download-folder-get" => folder_get(),
        _ => current_folder().and_then(|(dir, _)| run_in(&dir, method, params)),
    };
    envelope(outcome)
}

/// Dispatch des méthodes sur un dossier donné : les tests passent un dossier temporaire.
fn run_in(dir: &Path, method: &str, params: &Value) -> Result<Value, DlError> {
    let id = params.get("id").and_then(Value::as_str).unwrap_or("");
    match method {
        "download-open" => open(dir, id),
        "download-write" => {
            let offset = params
                .get("offset")
                .and_then(Value::as_u64)
                .ok_or(BAD_RANGE)?;
            let data = params.get("data").and_then(Value::as_str).ok_or(BAD_DATA)?;
            write_chunk(dir, id, offset, data)
        }
        "download-truncate" => {
            let len = params.get("len").and_then(Value::as_u64).ok_or(BAD_RANGE)?;
            truncate(dir, id, len)
        }
        "download-size" => size(dir, id),
        "download-finalize" => finalize(dir, id),
        "download-path" => locate(dir, id),
        "download-remove" => remove(dir, id),
        "download-usage" => usage(dir),
        _ => Err(IO),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Dossier temporaire unique, supprimé à la fin du test.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "laststream-dl-{tag}-{}-{nanos}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    const EBML: [u8; 8] = [0x1A, 0x45, 0xDF, 0xA3, 0x01, 0x00, 0x00, 0x00];
    const MP4: [u8; 12] = [
        0, 0, 0, 0x18, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm',
    ];

    #[test]
    fn ids_are_strictly_validated() {
        let long = "a".repeat(128);
        let too_long = "a".repeat(129);
        for ok in [
            "a",
            "A-b_c.d",
            "movie.2024.1080p.mkv",
            ".hidden",
            "COM10",
            "con-x",
            long.as_str(),
        ] {
            assert_eq!(validate_id(ok), Ok(ok), "{ok}");
        }
        for ko in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "a\\b",
            "C:x",
            "a b",
            "é",
            "a\0b",
            "a%2F",
            too_long.as_str(),
            "CON",
            "con.part",
            "Nul.mkv",
            "lpt1",
            "COM9.bin",
        ] {
            assert_eq!(validate_id(ko), Err(BAD_ID), "{ko}");
        }
    }

    #[test]
    fn invalid_ids_never_reach_the_disk() {
        let tmp = TempDir::new("badid");
        for method in [
            "download-open",
            "download-size",
            "download-finalize",
            "download-remove",
        ] {
            let reply = envelope(run_in(tmp.path(), method, &json!({ "id": ".." })));
            assert_eq!(reply["code"], "bad_id", "{method}");
            assert_eq!(reply["ok"], false);
        }
        let reply = envelope(run_in(tmp.path(), "download-open", &json!({})));
        assert_eq!(reply["code"], "bad_id");
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn write_then_resume_keeps_the_size() {
        let tmp = TempDir::new("resume");
        let dir = tmp.path();
        assert_eq!(open(dir, "film").unwrap()["size"], 0);
        assert_eq!(
            write_chunk(dir, "film", 0, &b64(b"hello")).unwrap()["size"],
            5
        );
        // Reprise : une nouvelle ouverture repart de la taille du .part, sans le tronquer.
        assert_eq!(open(dir, "film").unwrap()["size"], 5);
        assert_eq!(
            write_chunk(dir, "film", 5, &b64(b" world")).unwrap()["size"],
            11
        );
        assert_eq!(fs::read(dir.join("film.part")).unwrap(), b"hello world");
        assert_eq!(size(dir, "film").unwrap()["size"], 11);
        // Écriture au milieu : elle remplace les octets sans changer la taille.
        assert_eq!(write_chunk(dir, "film", 0, &b64(b"J")).unwrap()["size"], 11);
        assert_eq!(fs::read(dir.join("film.part")).unwrap(), b"Jello world");
    }

    #[test]
    fn offset_beyond_the_size_is_refused() {
        let tmp = TempDir::new("offset");
        let dir = tmp.path();
        write_chunk(dir, "film", 0, &b64(b"hello world")).unwrap();
        assert_eq!(write_chunk(dir, "film", 12, &b64(b"x")), Err(BAD_RANGE));
        assert_eq!(size(dir, "film").unwrap()["size"], 11);
        // Fichier neuf : seul l'offset 0 est accepté.
        assert_eq!(write_chunk(dir, "neuf", 1, &b64(b"x")), Err(BAD_RANGE));
        assert!(!dir.join("neuf.part").exists());
    }

    #[test]
    fn data_is_capped_at_sixteen_mebibytes_and_must_be_base64() {
        let tmp = TempDir::new("cap");
        let dir = tmp.path();
        let over = b64(&vec![0u8; MAX_WRITE_BYTES + 1]);
        assert_eq!(write_chunk(dir, "film", 0, &over), Err(TOO_LARGE));
        assert_eq!(
            write_chunk(dir, "film", 0, "pas du base64 !"),
            Err(BAD_DATA)
        );
        assert!(!dir.join("film.part").exists());
        // Exactement 16 Mio passent.
        let max = b64(&vec![7u8; MAX_WRITE_BYTES]);
        assert_eq!(
            write_chunk(dir, "film", 0, &max).unwrap()["size"],
            MAX_WRITE_BYTES as u64
        );
    }

    #[test]
    fn truncate_shrinks_and_refuses_to_grow() {
        let tmp = TempDir::new("truncate");
        let dir = tmp.path();
        write_chunk(dir, "film", 0, &b64(b"hello world")).unwrap();
        assert_eq!(truncate(dir, "film", 5).unwrap()["size"], 5);
        assert_eq!(fs::read(dir.join("film.part")).unwrap(), b"hello");
        assert_eq!(truncate(dir, "film", 10), Err(BAD_RANGE));
        assert_eq!(truncate(dir, "film", 0).unwrap()["size"], 0);
        assert_eq!(size(dir, "film").unwrap()["size"], 0);
    }

    #[test]
    fn finalize_names_the_file_by_its_signature() {
        let tmp = TempDir::new("finalize");
        let dir = tmp.path();
        write_chunk(dir, "a", 0, &b64(&EBML)).unwrap();
        write_chunk(dir, "b", 0, &b64(&MP4)).unwrap();
        write_chunk(dir, "c", 0, &b64(b"garbage")).unwrap();

        let a = finalize(dir, "a").unwrap();
        assert_eq!(a["ok"], true);
        assert_eq!(a["path"], dir.join("a.mkv").to_str().unwrap());
        assert_eq!(a["size"], EBML.len() as u64);
        assert!(!dir.join("a.part").exists());
        assert_eq!(
            finalize(dir, "b").unwrap()["path"],
            dir.join("b.mp4").to_str().unwrap()
        );
        assert_eq!(
            finalize(dir, "c").unwrap()["path"],
            dir.join("c.bin").to_str().unwrap()
        );

        // Appel répété : même fichier, pas d'erreur.
        assert_eq!(
            finalize(dir, "a").unwrap()["path"],
            dir.join("a.mkv").to_str().unwrap()
        );
        // Un fichier fini ne se réécrit pas, et `open` le signale.
        assert_eq!(write_chunk(dir, "a", 0, &b64(b"x")), Err(ALREADY_FINAL));
        let reopened = open(dir, "a").unwrap();
        assert_eq!(reopened["finalized"], true);
        assert_eq!(reopened["size"], EBML.len() as u64);
        assert_eq!(
            locate(dir, "b").unwrap()["path"],
            dir.join("b.mp4").to_str().unwrap()
        );
    }

    #[test]
    fn finalize_without_part_is_not_found() {
        let tmp = TempDir::new("nopart");
        assert_eq!(finalize(tmp.path(), "absent"), Err(NOT_FOUND));
        assert_eq!(locate(tmp.path(), "absent"), Err(NOT_FOUND));
    }

    #[test]
    fn stale_final_of_another_extension_is_replaced() {
        let tmp = TempDir::new("stale");
        let dir = tmp.path();
        fs::write(dir.join("v.bin"), b"ancienne version").unwrap();
        fs::write(dir.join("v.part"), EBML).unwrap();
        finalize(dir, "v").unwrap();
        assert!(dir.join("v.mkv").is_file());
        assert!(!dir.join("v.bin").exists());
    }

    #[test]
    fn remove_is_idempotent_and_clears_the_final_file() {
        let tmp = TempDir::new("remove");
        let dir = tmp.path();
        write_chunk(dir, "r", 0, &b64(b"abc")).unwrap();
        assert_eq!(remove(dir, "r").unwrap(), json!({ "ok": true }));
        assert_eq!(remove(dir, "r").unwrap(), json!({ "ok": true }));
        assert_eq!(size(dir, "r").unwrap()["size"], 0);
        assert_eq!(locate(dir, "r"), Err(NOT_FOUND));

        write_chunk(dir, "s", 0, &b64(&MP4)).unwrap();
        finalize(dir, "s").unwrap();
        remove(dir, "s").unwrap();
        assert!(!dir.join("s.mp4").exists());
    }

    #[test]
    fn usage_counts_the_folder_and_reports_free_space() {
        let tmp = TempDir::new("usage");
        let dir = tmp.path();
        fs::write(dir.join("a.part"), vec![0u8; 10]).unwrap();
        fs::write(dir.join("b.bin"), vec![0u8; 5]).unwrap();
        let report = usage(dir).unwrap();
        assert_eq!(report["ok"], true);
        assert_eq!(report["used"], 15);
        assert!(report["available"].as_u64().unwrap() > 0);
    }

    #[test]
    fn disk_full_is_reported_as_store_full() {
        assert_eq!(map_io(io::Error::from_raw_os_error(112)), STORE_FULL);
        assert_eq!(map_io(io::Error::from_raw_os_error(39)), STORE_FULL);
        assert_eq!(map_io(io::Error::from(io::ErrorKind::NotFound)), NOT_FOUND);
        assert_eq!(map_io(io::Error::from(io::ErrorKind::PermissionDenied)), IO);
    }

    #[test]
    fn signature_detection_picks_the_extension() {
        assert_eq!(detect_extension(&EBML), "mkv");
        assert_eq!(detect_extension(&MP4), "mp4");
        assert_eq!(detect_extension(b"ftyp"), "bin");
        assert_eq!(detect_extension(b"garbage!"), "bin");
        assert_eq!(detect_extension(&[]), "bin");
    }

    #[test]
    fn default_folder_is_videos_laststream_never_stremio() {
        let videos = PathBuf::from(r"C:\Users\x\Videos");
        assert_eq!(
            default_download_dir(Some(videos.clone()), None),
            Some(videos.join("Laststream"))
        );
        let home = PathBuf::from(r"C:\Users\x");
        assert_eq!(
            default_download_dir(None, Some(home.clone())),
            Some(home.join("Videos").join("Laststream"))
        );
        assert_eq!(default_download_dir(None, None), None);
        let text = default_download_dir(Some(videos), None)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(!text.contains("Stremio"));
    }

    #[test]
    fn configured_folder_is_recreated_or_refused() {
        let tmp = TempDir::new("folder");
        let wanted = tmp.path().join("sub").join("dossier");
        let (dir, is_default) = resolve_folder(wanted.to_str(), None).unwrap();
        assert!(dir.is_dir());
        assert!(!is_default);
        assert_eq!(dir, wanted);
        // Chemin relatif : ignoré, le dossier par défaut prend le relais.
        let (dir, is_default) =
            resolve_folder(Some("relatif"), Some(tmp.path().join("def"))).unwrap();
        assert!(is_default);
        assert!(dir.ends_with("def") && dir.is_dir());
        // Rien à utiliser : no_folder.
        assert_eq!(resolve_folder(None, None).err(), Some(NO_FOLDER));
        // Impossible à créer (un fichier barre le chemin) : no_folder.
        let blocker = tmp.path().join("fichier");
        fs::write(&blocker, b"x").unwrap();
        let under_file = blocker.join("x");
        assert_eq!(
            resolve_folder(under_file.to_str(), None).err(),
            Some(NO_FOLDER)
        );
    }

    #[test]
    fn unknown_method_and_missing_params_are_refused() {
        let tmp = TempDir::new("params");
        let dir = tmp.path();
        assert_eq!(run_in(dir, "download-inconnu", &json!({})), Err(IO));
        assert_eq!(write_chunk(dir, "film", 0, &b64(b"x")).unwrap()["ok"], true);
        assert_eq!(
            run_in(
                dir,
                "download-write",
                &json!({ "id": "film", "data": "eA==" })
            ),
            Err(BAD_RANGE)
        );
        assert_eq!(
            run_in(dir, "download-write", &json!({ "id": "film", "offset": 1 })),
            Err(BAD_DATA)
        );
        assert_eq!(
            run_in(dir, "download-truncate", &json!({ "id": "film" })),
            Err(BAD_RANGE)
        );
    }

    #[test]
    fn replies_carry_the_method_name_and_payload() {
        let wire: Value = serde_json::from_str(&RPCResponse::download_reply(
            "download-size",
            json!({ "ok": true, "size": 3 }),
        ))
        .unwrap();
        assert_eq!(wire["type"], 1);
        assert_eq!(wire["args"][0], "download-size");
        assert_eq!(wire["args"][1]["size"], 3);
        assert!(is_worker_method("download-finalize"));
        assert!(!is_worker_method("download-folder-pick"));
    }
}
