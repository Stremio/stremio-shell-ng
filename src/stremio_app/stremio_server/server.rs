use super::process::{ServerEvent, ServerProcess};
use crate::stremio_app::constants::STREMIO_SERVER_DEV_MODE;
use native_windows_gui::{self as nwg, PartialUi};
use std::{
    env, fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use winapi::um::winbase::CREATE_NO_WINDOW;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct StremioServer {
    pub notice: nwg::Notice,
    process: Option<ServerProcess>,
}

impl StremioServer {
    pub fn development(&self) -> bool {
        self.process.is_none()
    }

    pub fn start(&self) {
        if let Some(process) = &self.process {
            process.start();
        }
    }

    pub fn events(&self) -> impl Iterator<Item = ServerEvent> + '_ {
        self.process.iter().flat_map(ServerProcess::events)
    }
}

/// Fichiers de réglages (petits) repris de l'ancien dossier de données du serveur,
/// `%LOCALAPPDATA%\Stremio`, au premier lancement. Copie sans déplacer (le cache, volumineux,
/// est reconstruit) : une vraie installation de Stremio peut partager l'ancien dossier.
/// Ne remplace jamais un fichier déjà présent ; un échec de copie est retenté au lancement suivant.
fn adopt_legacy_server_data(dir: &Path, legacy: &Path) {
    const MAX_FILE_BYTES: u64 = 1024 * 1024;
    if !legacy.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(legacy) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let target = dir.join(entry.file_name());
        if meta.is_file() && meta.len() <= MAX_FILE_BYTES && !target.exists() {
            if fs::create_dir_all(dir).is_err() {
                return;
            }
            fs::copy(entry.path(), target).ok();
        }
    }
}

fn server_command() -> Result<Command, String> {
    super::job::protect_shell()
        .map_err(|error| format!("Impossible de superviser les processus enfants : {error}"))?;
    if let Some(base) = env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        adopt_legacy_server_data(&base.join("Laststream"), &base.join("Stremio"));
    }
    let mut path = env::current_exe().map_err(|error| {
        format!("Impossible de localiser l'installation de Laststream : {error}")
    })?;
    path.pop();
    let mut command = Command::new(path.join("laststream-runtime.exe"));
    command
        .arg(path.join("server.js"))
        .creation_flags(CREATE_NO_WINDOW);
    Ok(command)
}

impl PartialUi for StremioServer {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        let parent = parent.expect("No parent window").into();
        nwg::Notice::builder()
            .parent(parent)
            .build(&mut data.notice)?;

        if env::var(STREMIO_SERVER_DEV_MODE).as_deref() != Ok("true") {
            let sender = data.notice.sender();
            data.process = Some(ServerProcess::new(
                server_command,
                STARTUP_TIMEOUT,
                move || {
                    sender.notice();
                },
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::adopt_legacy_server_data;
    use std::{env, fs};

    #[test]
    fn legacy_server_settings_are_copied_once_without_the_cache() {
        let root = env::temp_dir().join(format!("laststream-srv-{}", uuid::Uuid::new_v4()));
        let legacy = root.join("Stremio");
        let dir = root.join("Laststream");
        fs::create_dir_all(legacy.join("stremio-cache")).unwrap();
        fs::write(legacy.join("server-settings.json"), "{}").unwrap();
        fs::write(legacy.join("big.bin"), vec![0u8; 1024 * 1024 + 1]).unwrap();

        adopt_legacy_server_data(&dir, &legacy);
        assert!(!dir.join("big.bin").exists());
        assert_eq!(
            fs::read_to_string(dir.join("server-settings.json")).unwrap(),
            "{}"
        );
        assert!(!dir.join("stremio-cache").exists());
        assert!(legacy.join("server-settings.json").exists());

        fs::write(dir.join("server-settings.json"), "nouveau").unwrap();
        adopt_legacy_server_data(&dir, &legacy);
        assert_eq!(
            fs::read_to_string(dir.join("server-settings.json")).unwrap(),
            "nouveau"
        );
        fs::remove_dir_all(root).ok();
    }
}
