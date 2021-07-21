use native_windows_gui::{self as nwg, NativeUi};
use std::ptr;
use structopt::StructOpt;
use winapi::um::wincon::GetConsoleWindow;
use winapi::um::winuser::{ShowWindow, SW_HIDE};

mod stremio_app;
use crate::stremio_app::{stremio_server::StremioServer, MainWindow};

const WEB_ENDPOINT: &str = "http://app.strem.io/shell-v4.4/";

#[derive(StructOpt, Debug)]
#[structopt(name = "basic")]
struct Opt {
    #[structopt(long)]
    development: bool,
    #[structopt(long, default_value = WEB_ENDPOINT)]
    webui_url: String,
}

fn main() {
    // Hide the terminal window
    let window = unsafe { GetConsoleWindow() };
    if window != ptr::null_mut() {
        unsafe {
            ShowWindow(window, SW_HIDE);
        }
    }

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

    let opt = Opt::from_args();

    let streaming_server: Option<StremioServer> = if opt.development {
        None
    } else {
        Some(StremioServer::new())
    };

    let webui_url = if opt.development && opt.webui_url == WEB_ENDPOINT {
        "http://localhost:11470".to_string()
    } else {
        opt.webui_url
    };

    nwg::init().expect("Failed to init Native Windows GUI");
    let _app = MainWindow::build_ui(MainWindow {
        webui_url,
        ..Default::default()
    })
    .expect("Failed to build UI");
    nwg::dispatch_thread_events();
    if let Some(streaming_server) = streaming_server {
        streaming_server.try_kill();
    }
}
