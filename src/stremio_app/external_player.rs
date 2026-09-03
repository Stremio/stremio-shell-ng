use std::{ffi::OsString, fs, os::windows::process::CommandExt, process::Command};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use url::Url;
use winapi::um::winbase::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW};

const M3U_DATA_URI_PREFIX: &str = "data:application/octet-stream;charset=utf-8;base64,";

pub fn play(input: &str) -> Result<(), &'static str> {
    let stream_url = parse_playlist(input)?;
    let directory = dirs::download_dir()
        .ok_or("Downloads directory is unavailable")?
        .join("Stremio");
    fs::create_dir_all(&directory).map_err(|_| "cannot create playlist directory")?;
    let path = directory.join("playlist.m3u");
    fs::write(&path, format!("#EXTM3U\n#EXTINF:0\n{stream_url}"))
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

fn parse_playlist(input: &str) -> Result<Url, &'static str> {
    let encoded = input
        .strip_prefix(M3U_DATA_URI_PREFIX)
        .ok_or("unsupported external player")?;
    let decoded = BASE64.decode(encoded).map_err(|_| "invalid playlist")?;
    let playlist = String::from_utf8(decoded).map_err(|_| "invalid playlist")?;
    let mut lines = playlist.lines();
    if lines.next() != Some("#EXTM3U") || lines.next() != Some("#EXTINF:0") {
        return Err("invalid playlist");
    }
    let url = lines.next().ok_or("invalid playlist")?;
    if lines.next().is_some() || url.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err("invalid playlist");
    }
    let url = Url::parse(url).map_err(|_| "invalid playlist")?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err("playlist URL must be absolute HTTP(S)");
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM_URL: &str = "https://example.com/video.mkv?token=secret%2Bvalue";

    fn data_uri(playlist: &str) -> String {
        format!("{M3U_DATA_URI_PREFIX}{}", BASE64.encode(playlist))
    }

    #[test]
    fn accepts_core_playlist() {
        let url = parse_playlist(&data_uri(&format!("#EXTM3U\n#EXTINF:0\n{STREAM_URL}"))).unwrap();
        assert_eq!(url.as_str(), STREAM_URL);
    }

    #[test]
    fn rejects_other_inputs() {
        for input in [
            "",
            "calc",
            "mpv://https://example.com/video.mkv",
            &data_uri("#EXTM3U\n#EXTINF:0\nfile:///C:/Windows/System32/calc.exe"),
            &data_uri("#EXTM3U\n#EXTINF:0\nhttps://example.com/one\nhttps://example.com/two"),
            &data_uri("#EXTM3U\n#EXTINF:-1\nhttps://example.com/video.mkv"),
            &data_uri("#EXTM3U\n#EXTINF:0\nhttps://example.com/a b"),
            "data:application/octet-stream;charset=utf-8;base64,not base64",
        ] {
            assert!(parse_playlist(input).is_err(), "{}", input);
        }
    }
}
