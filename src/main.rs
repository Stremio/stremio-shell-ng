#![cfg_attr(all(not(test), not(debug_assertions)), windows_subsystem = "windows")]
#[macro_use]
extern crate bitflags;
use std::{io::Write, path::Path, process::exit};
use url::Url;
use whoami::username;

use clap::Parser;
use native_windows_gui::{self as nwg, NativeUi};
mod laststream_app;
use crate::laststream_app::{
    constants::{DEV_ENDPOINT, IPC_PATH, LASTSTREAM_SERVER_DEV_MODE, SERVER_IPC_KEY},
    MainWindow, PipeClient,
};

#[derive(Parser, Debug)]
#[clap(version)]
struct Opt {
    command: Option<String>,
    #[clap(
        long,
        help = "Start the app only in system tray and keep the window hidden"
    )]
    start_hidden: bool,
    #[clap(long, help = "Do not show the splash image")]
    no_splash: bool,
    #[clap(
        long,
        alias = "dev-tools",
        help = "Enable the WebView2 developer tools in a release build (debug builds always have them)"
    )]
    devtools: bool,
    #[clap(long, help = "Disable the server and load the WebUI from localhost")]
    development: bool,
    #[clap(
        long,
        help = "Override the WebUI URL (default: the Laststream app served by the local service)"
    )]
    webui_url: Option<String>,
    #[clap(
        long,
        help = "Check this endpoint for updates instead of the default Laststream feed"
    )]
    autoupdater_endpoint: Option<Url>,
    #[clap(
        long,
        help = "Disable the autoupdater: no update check and no network call for it"
    )]
    no_autoupdater: bool,
    #[clap(long, help = "Forces reinstalling current version")]
    force_update: bool,
    #[clap(
        long,
        default_value = "",
        help = "Secret key for communication with the server. By default it is randomly generrated on startup"
    )]
    server_ipc_key: String,
}

/// `--webui-url` is loaded as given. Without it, `--development` loads the local dev server.
/// `None` keeps the Laststream app served by the local service.
fn resolve_webui_url(webui_url: Option<String>, development: bool) -> Option<String> {
    webui_url.or_else(|| development.then(|| DEV_ENDPOINT.to_string()))
}

fn main() {
    nwg::enable_visual_styles();

    let opt = Opt::parse();

    std::env::set_var(
        SERVER_IPC_KEY,
        if opt.server_ipc_key.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            opt.server_ipc_key.clone()
        },
    );

    let command = opt
        .command
        .filter(|command| !command.is_empty())
        .map(laststream_app::open_media::normalize)
        .unwrap_or_default();

    // Single application IPC
    let mut commands_path = IPC_PATH.to_string();
    // Append the username so it works per User
    commands_path.push_str(&username());
    let socket_path = Path::new(&commands_path);
    if let Ok(mut stream) = PipeClient::connect(socket_path) {
        let forwarded = stream
            .write_all(command.as_bytes())
            .and_then(|_| stream.flush())
            .is_ok();
        drop(stream);
        if forwarded {
            exit(0);
        }
        eprintln!(
            "Failed to forward command to existing Laststream instance; launching new instance"
        );
    }
    // END IPC

    std::env::set_var(
        LASTSTREAM_SERVER_DEV_MODE,
        if opt.development { "true" } else { "false" },
    );

    let webui_url = resolve_webui_url(opt.webui_url, opt.development);

    nwg::init().expect("Failed to init Native Windows GUI");
    let _app = MainWindow::build_ui(MainWindow {
        command,
        commands_path: Some(commands_path),
        webui_url,
        no_splash: opt.no_splash,
        dev_tools: opt.development || opt.devtools,
        start_hidden: opt.start_hidden,
        autoupdater_endpoint: laststream_app::updater::resolve_endpoint(
            opt.autoupdater_endpoint,
            opt.no_autoupdater || opt.development,
        ),
        force_update: opt.force_update,
        ..Default::default()
    })
    .expect("Failed to build UI");
    nwg::dispatch_thread_events();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devtools_option_is_opt_in_and_keeps_the_old_spelling() {
        assert!(
            Opt::try_parse_from(["laststream", "--devtools"])
                .unwrap()
                .devtools
        );
        assert!(
            Opt::try_parse_from(["laststream", "--dev-tools"])
                .unwrap()
                .devtools
        );
        assert!(!Opt::try_parse_from(["laststream"]).unwrap().devtools);
    }

    #[test]
    fn webui_url_option_is_loaded_as_given() {
        let url = "https://example.com/";
        assert_eq!(
            resolve_webui_url(Some(url.to_string()), false),
            Some(url.to_string())
        );
        assert_eq!(
            resolve_webui_url(Some(url.to_string()), true),
            Some(url.to_string())
        );
    }

    #[test]
    fn development_loads_the_dev_server_without_a_webui_url() {
        assert_eq!(
            resolve_webui_url(None, true),
            Some(DEV_ENDPOINT.to_string())
        );
    }

    #[test]
    fn without_options_the_local_service_serves_the_webui() {
        assert_eq!(resolve_webui_url(None, false), None);
    }
}
