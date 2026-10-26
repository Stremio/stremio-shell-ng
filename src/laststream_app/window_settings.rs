use serde::{Deserialize, Serialize};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};
use winapi::shared::windef::{HWND, POINT, RECT};
use winapi::um::winuser::{
    GetWindowPlacement, IsIconic, SW_SHOWMAXIMIZED, SW_SHOWNORMAL, WINDOWPLACEMENT,
};

const WINDOW_SETTINGS_FILE: &str = "window-state.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WindowSettings {
    show_cmd: u32,
    min_position: Point,
    max_position: Point,
    normal_position: Rect,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Point {
    x: i32,
    y: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl WindowSettings {
    pub fn load() -> Option<Self> {
        fs::read_to_string(settings_path())
            .ok()
            .and_then(|settings| serde_json::from_str(&settings).ok())
    }

    pub fn save(hwnd: HWND) -> io::Result<()> {
        let Some(settings) = Self::from_window(hwnd) else {
            return Ok(());
        };
        let path = settings_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&settings).map_err(io::Error::other)?;
        fs::write(path, json)
    }

    pub fn to_window_placement(&self) -> WINDOWPLACEMENT {
        let mut placement = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            flags: 0,
            showCmd: self.show_cmd,
            ptMinPosition: self.min_position.clone().into(),
            ptMaxPosition: self.max_position.clone().into(),
            rcNormalPosition: self.normal_position.clone().into(),
        };
        if !is_restorable_size(&placement.rcNormalPosition) {
            placement.showCmd = SW_SHOWNORMAL as u32;
        }
        placement
    }

    fn from_window(hwnd: HWND) -> Option<Self> {
        if unsafe { IsIconic(hwnd) } != 0 {
            return None;
        }

        let mut placement = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            flags: 0,
            showCmd: 0,
            ptMinPosition: POINT { x: 0, y: 0 },
            ptMaxPosition: POINT { x: 0, y: 0 },
            rcNormalPosition: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
        };

        if unsafe { GetWindowPlacement(hwnd, &mut placement) } == 0 {
            return None;
        }
        if !is_restorable_size(&placement.rcNormalPosition) {
            return None;
        }

        Some(WindowSettings {
            show_cmd: if placement.showCmd == SW_SHOWMAXIMIZED as u32 {
                SW_SHOWMAXIMIZED as u32
            } else {
                SW_SHOWNORMAL as u32
            },
            min_position: placement.ptMinPosition.into(),
            max_position: placement.ptMaxPosition.into(),
            normal_position: placement.rcNormalPosition.into(),
        })
    }
}

fn settings_path() -> PathBuf {
    let base = env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    let path = base.join("Laststream").join(WINDOW_SETTINGS_FILE);
    adopt_legacy_settings(&path, &base.join("Stremio").join(WINDOW_SETTINGS_FILE));
    path
}

/// Reprend l'état de fenêtre de l'ancien dossier `%APPDATA%\Stremio` au premier lancement.
/// Copie sans déplacer : une vraie installation de Stremio peut partager ce dossier.
fn adopt_legacy_settings(path: &Path, legacy: &Path) {
    if path.exists() || !legacy.is_file() {
        return;
    }
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    fs::copy(legacy, path).ok();
}

fn is_restorable_size(rect: &RECT) -> bool {
    rect.right > rect.left && rect.bottom > rect.top
}

impl From<POINT> for Point {
    fn from(point: POINT) -> Self {
        Point {
            x: point.x,
            y: point.y,
        }
    }
}

impl From<Point> for POINT {
    fn from(point: Point) -> Self {
        POINT {
            x: point.x,
            y: point.y,
        }
    }
}

impl From<RECT> for Rect {
    fn from(rect: RECT) -> Self {
        Rect {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }
}

impl From<Rect> for RECT {
    fn from(rect: Rect) -> Self {
        RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{adopt_legacy_settings, is_restorable_size, WINDOW_SETTINGS_FILE};
    use std::{env, fs, path::PathBuf};
    use winapi::shared::windef::RECT;

    fn scratch(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("laststream-ws-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn legacy_window_state_is_copied_once() {
        let root = scratch("copy");
        let legacy = root.join("Stremio").join(WINDOW_SETTINGS_FILE);
        let path = root.join("Laststream").join(WINDOW_SETTINGS_FILE);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, "ancien").unwrap();

        adopt_legacy_settings(&path, &legacy);
        assert_eq!(fs::read_to_string(&path).unwrap(), "ancien");
        assert!(legacy.exists(), "l'ancien fichier reste en place");

        fs::write(&path, "nouveau").unwrap();
        adopt_legacy_settings(&path, &legacy);
        assert_eq!(fs::read_to_string(&path).unwrap(), "nouveau");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn missing_legacy_window_state_creates_nothing() {
        let root = scratch("none");
        let path = root.join("Laststream").join(WINDOW_SETTINGS_FILE);
        adopt_legacy_settings(&path, &root.join("Stremio").join(WINDOW_SETTINGS_FILE));
        assert!(!path.exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_empty_window_rect() {
        assert!(!is_restorable_size(&RECT {
            left: 10,
            top: 10,
            right: 10,
            bottom: 20,
        }));
    }

    #[test]
    fn accepts_non_empty_window_rect() {
        assert!(is_restorable_size(&RECT {
            left: 10,
            top: 10,
            right: 20,
            bottom: 20,
        }));
    }
}
