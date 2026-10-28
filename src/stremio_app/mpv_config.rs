use std::{fs, io, path::PathBuf};

const STORED_PATH_FILE: &str = "mpv-config.txt";

fn settings_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("Stremio"))
}

pub fn path() -> Option<String> {
    settings_dir()
        .and_then(|dir| fs::read_to_string(dir.join(STORED_PATH_FILE)).ok())
        .filter(|path| !path.is_empty())
}

pub fn set(path: &str) -> io::Result<()> {
    let dir = settings_dir().ok_or(io::ErrorKind::NotFound)?;
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(STORED_PATH_FILE), path)
}
