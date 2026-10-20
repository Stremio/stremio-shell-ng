#![cfg_attr(all(not(test), not(debug_assertions)), windows_subsystem = "windows")]
#[macro_use]
extern crate bitflags;
use std::{io::Write, path::Path, process::exit};
use url::Url;
use whoami::username;

use clap::Parser;
use native_windows_gui::{self as nwg, NativeUi};
mod stremio_app;
use crate::stremio_app::{
    constants::{
        DEV_ENDPOINT, IPC_PATH, SERVER_IPC_KEY, STA_ENDPOINT, STREMIO_SERVER_DEV_MODE, WEB_ENDPOINT,
    },
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
        help = "Deprecated: dev tools are always available from the context menu"
    )]
    dev_tools: bool,
    #[clap(long, help = "Disable the server and load the WebUI from localhost")]
    development: bool,
    #[clap(long, help = "Shortcut for --webui-url=https://staging.strem.io/")]
    staging: bool,
    #[clap(
        long,
        help = "Override the WebUI URL (default: the Laststream app served by the local service)"
    )]
    webui_url: Option<String>,
    #[clap(
        long,
        help = "Enable the autoupdater and check this endpoint (no update check without it)"
    )]
    autoupdater_endpoint: Option<Url>,
    #[clap(long, help = "Forces reinstalling current version")]
    force_update: bool,
    #[clap(
        long,
        default_value = "",
        help = "Secret key for communication with the server. By default it is randomly generrated on startup"
    )]
    server_ipc_key: String,
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
        .map(stremio_app::open_media::normalize)
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
        eprintln!("Failed to forward command to existing Stremio instance; launching new instance");
    }
    // END IPC

    std::env::set_var(
        STREMIO_SERVER_DEV_MODE,
        if opt.development { "true" } else { "false" },
    );

    // None keeps the Laststream app served by the local service.
    let webui_url = match opt.webui_url.as_deref() {
        Some(url) if url != WEB_ENDPOINT => Some(url.to_string()),
        _ if opt.development => Some(DEV_ENDPOINT.to_string()),
        _ if opt.staging => Some(STA_ENDPOINT.to_string()),
        explicit => explicit.map(str::to_string),
    };

    nwg::init().expect("Failed to init Native Windows GUI");
    let _app = MainWindow::build_ui(MainWindow {
        command,
        commands_path: Some(commands_path),
        webui_url,
        no_splash: opt.no_splash,
        dev_tools: opt.development || opt.dev_tools,
        start_hidden: opt.start_hidden,
        autoupdater_endpoint: opt.autoupdater_endpoint,
        force_update: opt.force_update,
        ..Default::default()
    })
    .expect("Failed to build UI");
    nwg::dispatch_thread_events();
}
