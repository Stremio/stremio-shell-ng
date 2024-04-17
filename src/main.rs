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
    constants::{DEV_ENDPOINT, IPC_PATH, STA_ENDPOINT, WEB_ENDPOINT},
    stremio_server::StremioServer,
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
    #[clap(long, help = "Enable dev tools when pressing F12")]
    dev_tools: bool,
    #[clap(long, help = "Disable the server and load the WebUI from localhost")]
    development: bool,
    #[clap(long, help = "Shortcut for --webui-url=https://staging.strem.io/")]
    staging: bool,
    #[clap(long, default_value = WEB_ENDPOINT, help = "Override the WebUI URL")]
    webui_url: String,
    #[clap(long, help = "Ovveride autoupdater endpoint")]
    autoupdater_endpoint: Option<Url>,
    #[clap(long, help = "Forces reinstalling current version")]
    force_update: bool,
    #[clap(long, help = "Check for RC updates")]
    release_candidate: bool,
}

fn main() {
    // native-windows-gui has some basic high DPI support with the high-dpi
    // feature. It supports the "System DPI Awareness" mode, but not the more
    // advanced Per-Monitor (v2) DPI Awareness modes.
    //
    // Use an application manifest to get rid of this deprecated warning.
    #[allow(deprecated)]
    unsafe {
        nwg::set_dpi_awareness()
    };
    nwg::enable_visual_styles();

    let opt = Opt::parse();

    let command = match opt.command {
        Some(file) => {
            if Path::new(&file).exists() {
                "file:///".to_string() + &file.replace('\\', "/")
            } else {
                file
            }
        }
        None => "".to_string(),
    };

    // Single application IPC
    let mut commands_path = IPC_PATH.to_string();
    // Append the username so it works per User
    commands_path.push_str(&username());
    let socket_path = Path::new(&commands_path);
    if let Ok(mut stream) = PipeClient::connect(socket_path) {
        stream.write_all(command.as_bytes()).ok();
        exit(0);
    }
    // END IPC

    if !opt.development {
        StremioServer::new();
    }

    let webui_url = if opt.development && opt.webui_url == WEB_ENDPOINT {
        DEV_ENDPOINT.to_string()
    } else if opt.staging && opt.webui_url == WEB_ENDPOINT {
        STA_ENDPOINT.to_string()
    } else {
        opt.webui_url
    };

    nwg::init().expect("Failed to init Native Windows GUI");
    let _app = MainWindow::build_ui(MainWindow {
        command,
        commands_path: Some(commands_path),
        webui_url,
        dev_tools: opt.development || opt.dev_tools,
        start_hidden: opt.start_hidden,
        autoupdater_endpoint: opt.autoupdater_endpoint,
        force_update: opt.force_update,
        release_candidate: opt.release_candidate,
        ..Default::default()
    })
    .expect("Failed to build UI");
    nwg::dispatch_thread_events();
}
