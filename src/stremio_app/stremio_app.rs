use native_windows_derive::NwgUi;
use native_windows_gui as nwg;

use crate::stremio_app::stremio_wevbiew::WebView;
use crate::stremio_app::stremio_player::Player;

#[derive(Default, NwgUi)]
pub struct StremioApp {
    #[nwg_control(title: "Stremio", flags: "MAIN_WINDOW|VISIBLE")]
    #[nwg_events( OnWindowClose: [StremioApp::quit] )]
    window: nwg::Window,
    #[nwg_partial(parent: window)]
    webview: WebView,
    #[nwg_partial(parent: window)]
    player: Player,
}

impl StremioApp {
    fn quit(&self) {
        nwg::stop_thread_dispatch();
    }
}
