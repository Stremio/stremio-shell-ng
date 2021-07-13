use std::cmp;
use native_windows_derive::NwgUi;
use native_windows_gui as nwg;

use crate::stremio_app::stremio_wevbiew::WebView;
use crate::stremio_app::stremio_player::Player;

#[derive(Default, NwgUi)]
pub struct StremioApp {
    #[nwg_control(title: "Stremio", flags: "MAIN_WINDOW|VISIBLE")]
    #[nwg_events( OnWindowClose: [StremioApp::on_quit], OnInit: [StremioApp::on_init] )]
    window: nwg::Window,
    #[nwg_partial(parent: window)]
    webview: WebView,
    #[nwg_partial(parent: window)]
    player: Player,
}

impl StremioApp {
    fn on_init(&self) {
        let small_side = cmp::min(nwg::Monitor::width(), nwg::Monitor::height()) * 70 / 100;
        let dimensions = (small_side * 16 / 9, small_side);
        let [total_width, total_height] = [nwg::Monitor::width(), nwg::Monitor::height()];
        let x = (total_width-dimensions.0)/2;
        let y = (total_height-dimensions.1)/2;
        self.window.set_size(dimensions.0 as u32, dimensions.1 as u32);
        self.window.set_position(x, y);
    }
    fn on_quit(&self) {
        nwg::stop_thread_dispatch();
    }
}
