use std::{
    env, ffi::OsString, fs, os::windows::process::CommandExt, path::PathBuf, process::Command,
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use native_windows_gui as nwg;
use url::Url;
use winapi::um::winbase::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW};
use winreg::{
    enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
    RegKey,
};

use crate::stremio_app::constants::APP_NAME;

const M3U_DATA_URI_PREFIX: &str = "data:application/octet-stream;charset=utf-8;base64,";
const APP_PATHS: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths";

#[derive(Debug, PartialEq)]
enum Request {
    Vlc(Url),
    Playlist(Url),
}

pub fn play(input: &str) -> Result<(), &'static str> {
    match parse(input)? {
        Request::Vlc(url) => spawn_player("VLC", "vlc.exe", r"VideoLAN\VLC\vlc.exe", &url),
        Request::Playlist(url) => open_playlist(&url),
    }
}

fn parse(input: &str) -> Result<Request, &'static str> {
    if let Some(encoded) = input.strip_prefix(M3U_DATA_URI_PREFIX) {
        return parse_playlist(encoded).map(Request::Playlist);
    }
    let (scheme, rest) = input
        .split_once("://")
        .ok_or("unsupported external player")?;
    match scheme.to_ascii_lowercase().as_str() {
        "vlc" => stream_url(rest).map(Request::Vlc),
        _ => Err("unsupported external player"),
    }
}

fn parse_playlist(encoded: &str) -> Result<Url, &'static str> {
    let decoded = BASE64.decode(encoded).map_err(|_| "invalid playlist")?;
    let playlist = String::from_utf8(decoded).map_err(|_| "invalid playlist")?;
    let mut lines = playlist.lines();
    if lines.next() != Some("#EXTM3U") || lines.next() != Some("#EXTINF:0") {
        return Err("invalid playlist");
    }
    let url = lines.next().ok_or("invalid playlist")?;
    if lines.next().is_some() {
        return Err("invalid playlist");
    }
    stream_url(url)
}

fn stream_url(input: &str) -> Result<Url, &'static str> {
    const ERROR: &str = "stream URL must be absolute HTTP(S)";
    if input.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(ERROR);
    }
    let url = Url::parse(input).map_err(|_| ERROR)?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err(ERROR);
    }
    Ok(url)
}

fn spawn_player(name: &str, exe: &str, install_path: &str, url: &Url) -> Result<(), &'static str> {
    let paths = player_paths(exe, install_path);
    let spawned = paths.iter().find_map(|path| {
        Command::new(path)
            .arg("--")
            .arg(url.as_str())
            .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
            .spawn()
            .ok()
    });
    if spawned.is_none() {
        eprintln!("{name} was not found in {paths:?}");
        nwg::error_message(
            APP_NAME,
            &format!("{name} was not found on this computer. Install it and try again."),
        );
        return Err("external player is not installed");
    }
    Ok(())
}

fn player_paths(exe: &str, install_path: &str) -> Vec<PathBuf> {
    let app_paths = [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
        .iter()
        .filter_map(|hive| {
            RegKey::predef(*hive)
                .open_subkey(format!(r"{APP_PATHS}\{exe}"))
                .and_then(|key| key.get_value::<String, _>(""))
                .ok()
        })
        .map(|path| PathBuf::from(path.trim_matches('"')));
    let open_with = RegKey::predef(HKEY_CLASSES_ROOT)
        .open_subkey(format!(r"Applications\{exe}\shell\open\command"))
        .and_then(|key| key.get_value::<String, _>(""))
        .ok()
        .and_then(|command| command_program(&command));
    let default_dirs = ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(env::var_os)
        .map(|dir| PathBuf::from(dir).join(install_path));
    app_paths
        .chain(open_with)
        .chain(default_dirs)
        .chain([PathBuf::from(exe)])
        .collect()
}

fn command_program(command: &str) -> Option<PathBuf> {
    let program = match command.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next()?,
        None => command.split(' ').next()?,
    };
    (!program.is_empty()).then(|| PathBuf::from(program))
}

fn open_playlist(url: &Url) -> Result<(), &'static str> {
    let directory = dirs::download_dir()
        .ok_or("Downloads directory is unavailable")?
        .join("Stremio");
    fs::create_dir_all(&directory).map_err(|_| "cannot create playlist directory")?;
    let path = directory.join("playlist.m3u");
    fs::write(&path, format!("#EXTM3U\n#EXTINF:0\n{url}"))
        .map_err(|_| "cannot save M3U playlist")?;
    let mut quoted_path = OsString::from("\"");
    quoted_path.push(&path);
    quoted_path.push("\"");
    Command::new("cmd")
        .args(["/C", "start", ""])
        .raw_arg(quoted_path)
        .creation_flags(CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW)
        .spawn()
        .map(drop)
        .map_err(|_| "cannot open M3U playlist")
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM_URL: &str = "https://example.com/video.mkv?token=secret%2Bvalue";

    fn data_uri(playlist: &str) -> String {
        format!("{}{}", M3U_DATA_URI_PREFIX, BASE64.encode(playlist))
    }

    #[test]
    fn accepts_core_forms() {
        let stream = Url::parse(STREAM_URL).unwrap();
        assert_eq!(
            parse(&format!("vlc://{}", STREAM_URL)),
            Ok(Request::Vlc(stream.clone()))
        );
        assert_eq!(
            parse(&data_uri(&format!("#EXTM3U\n#EXTINF:0\n{}", STREAM_URL))),
            Ok(Request::Playlist(stream))
        );
    }

    #[test]
    fn reads_program_from_open_with_command() {
        assert_eq!(
            command_program(r#""C:\Program Files\VideoLAN\VLC\vlc.exe" "%1""#),
            Some(PathBuf::from(r"C:\Program Files\VideoLAN\VLC\vlc.exe"))
        );
        assert_eq!(
            command_program(r#""C:\VLC\vlc.exe" -- "%L""#),
            Some(PathBuf::from(r"C:\VLC\vlc.exe"))
        );
        assert_eq!(
            command_program(r"C:\VLC\vlc.exe %1"),
            Some(PathBuf::from(r"C:\VLC\vlc.exe"))
        );
        assert_eq!(command_program(""), None);
    }

    #[test]
    fn rejects_unsafe_values() {
        for input in [
            "",
            "calc",
            "file:///C:/Windows/System32/calc.exe",
            "mpv://https://example.com/video.mkv",
            "vlc://file:///C:/Windows/System32/calc.exe",
            "vlc://https://example.com/video.mkv\n--script=attack.lua",
            "vlc://javascript:alert(1)",
            "potplayer://https://example.com/video.mkv",
            "iina://weblink?url=https%3A%2F%2Fexample.com%2Fvideo.mkv",
            &data_uri("#EXTM3U\n#EXTINF:0\nfile:///C:/Windows/System32/calc.exe"),
            &data_uri("#EXTM3U\n#EXTINF:0\nhttps://example.com/one\nhttps://example.com/two"),
            &data_uri("#EXTM3U\n#EXTINF:-1\nhttps://example.com/video.mkv"),
            "data:application/octet-stream;charset=utf-8;base64,not base64",
        ] {
            assert!(parse(input).is_err(), "{}", input);
        }
    }
}
