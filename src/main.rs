#![windows_subsystem = "windows"]
#[macro_use]
extern crate bitflags;
use std::io::Write;
use std::path::Path;
use std::process::exit;
use whoami::username;

use clap::Parser;
use native_windows_gui::{self as nwg, NativeUi};
mod stremio_app;
use crate::stremio_app::{stremio_server::StremioServer, MainWindow, PipeClient};

const DEV_ENDPOINT: &str = "http://127.0.0.1:11470";
const WEB_ENDPOINT: &str = "https://app.strem.io/shell-v4.4/";
const STA_ENDPOINT: &str = "https://staging.strem.io/";

#[derive(Parser, Debug)]
#[clap(version)]
struct Opt {
    command: Option<String>,
    #[clap(long, help = "Enable dev tools when pressing F12")]
    dev_tools: bool,
    #[clap(long, help = "Disable the server and load the WebUI from localhost")]
    development: bool,
    #[clap(long, help = "Shortcut for --webui-url=https://staging.strem.io/")]
    staging: bool,
    #[clap(long, default_value = WEB_ENDPOINT, help = "Override the WebUI URL")]
    webui_url: String,
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
    let mut commands_path = "//./pipe/com.stremio5.".to_string();
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
        ..Default::default()
    })
    .expect("Failed to build UI");
    nwg::dispatch_thread_events();
}
