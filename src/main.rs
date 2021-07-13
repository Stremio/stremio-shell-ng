use native_windows_gui::{self as nwg, NativeUi};

mod stremio_app;
use crate::stremio_app::{stremio_server::StremioServer, StremioApp};

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

    let streaming_server = StremioServer::new();

    nwg::init().expect("Failed to init Native Windows GUI");
    let _app = StremioApp::build_ui(Default::default()).expect("Failed to build UI");
    nwg::dispatch_thread_events();
    streaming_server.try_kill();
}
