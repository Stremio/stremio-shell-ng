use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use std::cmp;

use crate::stremio_app::stremio_player::{Player, PlayerInterface};
use crate::stremio_app::stremio_wevbiew::WebView;

#[derive(Default, NwgUi)]
pub struct MainWindow {
    #[nwg_control(title: "Stremio", flags: "MAIN_WINDOW|VISIBLE")]
    #[nwg_events( OnWindowClose: [MainWindow::on_quit], OnInit: [MainWindow::on_init] )]
    window: nwg::Window,
    #[nwg_partial(parent: window)]
    webview: WebView,
    #[nwg_partial(parent: window)]
    player: Player,
}

impl MainWindow {
    fn on_init(&self) {
        let small_side = cmp::min(nwg::Monitor::width(), nwg::Monitor::height()) * 70 / 100;
        let dimensions = (small_side * 16 / 9, small_side);
        let [total_width, total_height] = [nwg::Monitor::width(), nwg::Monitor::height()];
        let x = (total_width - dimensions.0) / 2;
        let y = (total_height - dimensions.1) / 2;
        self.window
            .set_size(dimensions.0 as u32, dimensions.1 as u32);
        self.window.set_position(x, y);
        // let video_path = "/home/ivo/storage/bbb_sunflower_1080p_30fps_normal.mp4";
        let video_path = "http://distribution.bbb3d.renderfarming.net/video/mp4/bbb_sunflower_1080p_30fps_normal.mp4";
        self.player.play(video_path);
        // self.player.seek(120.0);
        self.player.speed(2.0);
        // self.player.pause(true);
    }
    fn on_quit(&self) {
        nwg::stop_thread_dispatch();
    }
}
