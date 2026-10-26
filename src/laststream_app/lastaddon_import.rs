//! First-run import of the developer instance's data into the Laststream app data folder.
//!
//! Copies `data/laststream.db` with its WAL files, and the `.env` holding `LASTADDON_SECRET`, so
//! the account, profiles, library and progress come along and the encrypted configurations stay
//! valid. The import runs only while the target holds none of these files, and never overwrites.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

/// The developer checkout; `LASTADDON_IMPORT_FROM` replaces it, and an empty value disables the import.
const DEFAULT_IMPORT_SOURCE: &str = r"C:\Users\grego\Projets\laststream\lastaddon";
const IMPORT_SOURCE_VAR: &str = "LASTADDON_IMPORT_FROM";
const STAGING_DIR: &str = ".import-staging";

/// (path relative to the source root, file name in the app data folder).
const IMPORTED_FILES: [(&str, &str); 4] = [
    ("data/laststream.db", "laststream.db"),
    ("data/laststream.db-wal", "laststream.db-wal"),
    ("data/laststream.db-shm", "laststream.db-shm"),
    (".env", ".env"),
];

/// Any of these in the target means it already holds data: the import does nothing.
const TARGET_MARKERS: [&str; 4] = [
    "laststream.db",
    "laststream.db-wal",
    "laststream.db-shm",
    ".env",
];

/// The developer checkout to import from, or `None` when the import is disabled.
pub fn import_source() -> Option<PathBuf> {
    match env::var_os(IMPORT_SOURCE_VAR) {
        None => Some(PathBuf::from(DEFAULT_IMPORT_SOURCE)),
        Some(value) if value.is_empty() => None,
        Some(value) => Some(PathBuf::from(value)),
    }
}

/// Copies the developer data into `target`, when `target` holds none of it yet.
///
/// Returns whether anything was copied. Everything is copied to a staging folder first and moved
/// into place only once every copy succeeded, so a failure leaves the target as it was.
pub fn import_developer_data(source: &Path, target: &Path) -> io::Result<bool> {
    if TARGET_MARKERS.iter().any(|name| target.join(name).exists()) {
        return Ok(false);
    }
    let files: Vec<(PathBuf, &str)> = IMPORTED_FILES
        .iter()
        .map(|(from, to)| (source.join(from), *to))
        .filter(|(path, _)| path.is_file())
        .collect();
    if files.is_empty() {
        return Ok(false);
    }

    fs::create_dir_all(target)?;
    let staging = target.join(STAGING_DIR);
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;
    let result = stage_and_move(&files, &staging, target);
    fs::remove_dir_all(&staging).ok();
    result.map(|()| true)
}

fn stage_and_move(files: &[(PathBuf, &str)], staging: &Path, target: &Path) -> io::Result<()> {
    for (from, name) in files {
        fs::copy(from, staging.join(name))?;
    }
    for (_, name) in files {
        if target.join(name).exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the target already holds this file",
            ));
        }
    }
    for (_, name) in files {
        fs::rename(staging.join(name), target.join(name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A unique folder under the system temp directory, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("System clock")
                .as_nanos();
            let path = env::temp_dir().join(format!(
                "lastaddon-import-{tag}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("Create temp folder");
            Self(path)
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

    /// A developer checkout with the layout of the lastAddon project.
    fn developer_checkout(root: &Path) {
        fs::create_dir_all(root.join("data")).expect("Create data folder");
        fs::write(root.join("data").join("laststream.db"), b"main").unwrap();
        fs::write(root.join("data").join("laststream.db-wal"), b"wal").unwrap();
        fs::write(root.join("data").join("laststream.db-shm"), b"shm").unwrap();
        fs::write(root.join(".env"), b"LASTADDON_SECRET=test-secret").unwrap();
    }

    #[test]
    fn copies_database_wal_and_env_into_an_empty_folder() {
        let dir = TempDir::new("copy");
        let source = dir.path().join("checkout");
        let target = dir.path().join("app-data");
        developer_checkout(&source);

        assert!(import_developer_data(&source, &target).unwrap());
        assert_eq!(fs::read(target.join("laststream.db")).unwrap(), b"main");
        assert_eq!(fs::read(target.join("laststream.db-wal")).unwrap(), b"wal");
        assert_eq!(fs::read(target.join("laststream.db-shm")).unwrap(), b"shm");
        assert_eq!(
            fs::read(target.join(".env")).unwrap(),
            b"LASTADDON_SECRET=test-secret"
        );
        assert!(!target.join(STAGING_DIR).exists());

        // The second run finds the data it copied and does nothing.
        assert!(!import_developer_data(&source, &target).unwrap());
    }

    #[test]
    fn never_overwrites_existing_app_data() {
        let dir = TempDir::new("keep");
        let source = dir.path().join("checkout");
        let target = dir.path().join("app-data");
        developer_checkout(&source);
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("laststream.db"), b"mine").unwrap();

        assert!(!import_developer_data(&source, &target).unwrap());
        assert_eq!(fs::read(target.join("laststream.db")).unwrap(), b"mine");
        assert!(!target.join(".env").exists());
        assert!(!target.join("laststream.db-wal").exists());
    }

    #[test]
    fn an_existing_env_blocks_the_import_so_the_secret_is_not_replaced() {
        let dir = TempDir::new("env");
        let source = dir.path().join("checkout");
        let target = dir.path().join("app-data");
        developer_checkout(&source);
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join(".env"), b"LASTADDON_SECRET=generated").unwrap();

        assert!(!import_developer_data(&source, &target).unwrap());
        assert_eq!(
            fs::read(target.join(".env")).unwrap(),
            b"LASTADDON_SECRET=generated"
        );
        assert!(!target.join("laststream.db").exists());
    }

    #[test]
    fn copies_only_the_files_the_source_has() {
        let dir = TempDir::new("partial");
        let source = dir.path().join("checkout");
        let target = dir.path().join("app-data");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join(".env"), b"LASTADDON_SECRET=only-env").unwrap();

        assert!(import_developer_data(&source, &target).unwrap());
        assert_eq!(
            fs::read(target.join(".env")).unwrap(),
            b"LASTADDON_SECRET=only-env"
        );
        assert!(!target.join("laststream.db").exists());
    }

    #[test]
    fn does_nothing_when_the_source_has_no_data() {
        let dir = TempDir::new("empty");
        let source = dir.path().join("missing-checkout");
        let target = dir.path().join("app-data");

        assert!(!import_developer_data(&source, &target).unwrap());
        assert!(!target.exists());
    }

    #[test]
    fn the_source_comes_from_the_environment_and_can_be_disabled() {
        env::remove_var(IMPORT_SOURCE_VAR);
        assert_eq!(import_source(), Some(PathBuf::from(DEFAULT_IMPORT_SOURCE)));

        env::set_var(IMPORT_SOURCE_VAR, "");
        assert_eq!(import_source(), None);

        env::set_var(IMPORT_SOURCE_VAR, r"D:\other\lastaddon");
        assert_eq!(import_source(), Some(PathBuf::from(r"D:\other\lastaddon")));
        env::remove_var(IMPORT_SOURCE_VAR);
    }
}
