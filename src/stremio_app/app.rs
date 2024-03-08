use crate::stremio_app::PipeServer;
use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use serde_json;
use std::cell::RefCell;
use std::io::Read;
use std::path::Path;
use std::str;
use std::thread;
use winapi::um::winuser::WS_EX_TOPMOST;

use crate::stremio_app::ipc::{RPCRequest, RPCResponse};
use crate::stremio_app::splash::SplashImage;
use crate::stremio_app::stremio_player::Player;
use crate::stremio_app::stremio_wevbiew::WebView;
use crate::stremio_app::systray::SystemTray;
use crate::stremio_app::window_helper::WindowStyle;

#[derive(Default, NwgUi)]
pub struct MainWindow {
    pub command: String,
    pub commands_path: Option<String>,
    pub webui_url: String,
    pub dev_tools: bool,
    pub saved_window_style: RefCell<WindowStyle>,
    #[nwg_resource]
    pub embed: nwg::EmbedResource,
    #[nwg_resource(source_embed: Some(&data.embed), source_embed_str: Some("MAINICON"))]
    pub window_icon: nwg::Icon,
    #[nwg_control(icon: Some(&data.window_icon), title: "Stremio", flags: "MAIN_WINDOW|VISIBLE")]
    #[nwg_events( OnWindowClose: [Self::on_quit(SELF, EVT_DATA)], OnInit: [Self::on_init], OnPaint: [Self::on_paint], OnMinMaxInfo: [Self::on_min_max(SELF, EVT_DATA)], OnWindowMinimize: [Self::transmit_window_state_change] )]
    pub window: nwg::Window,
    #[nwg_partial(parent: window)]
    #[nwg_events((tray_exit, OnMenuItemSelected): [nwg::stop_thread_dispatch()], (tray_show_hide, OnMenuItemSelected): [Self::on_show_hide], (tray_topmost, OnMenuItemSelected): [Self::on_toggle_topmost]) ]
    pub tray: SystemTray,
    #[nwg_partial(parent: window)]
    pub webview: WebView,
    #[nwg_partial(parent: window)]
    pub player: Player,
    #[nwg_partial(parent: window)]
    pub splash_screen: SplashImage,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_toggle_fullscreen_notice] )]
    pub toggle_fullscreen_notice: nwg::Notice,
    #[nwg_control]
    #[nwg_events(OnNotice: [nwg::stop_thread_dispatch()] )]
    pub quit_notice: nwg::Notice,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_hide_splash_notice] )]
    pub hide_splash_notice: nwg::Notice,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_focus_notice] )]
    pub focus_notice: nwg::Notice,
}

impl MainWindow {
    const MIN_WIDTH: i32 = 1000;
    const MIN_HEIGHT: i32 = 600;
    fn transmit_window_full_screen_change(&self, prevent_close: bool) {
        let web_channel = self.webview.channel.borrow();
        let (web_tx, _) = web_channel
            .as_ref()
            .expect("Cannont obtain communication channel for the Web UI");
        let web_tx_app = web_tx.clone();
        let saved_style = self.saved_window_style.borrow();
        web_tx_app
            .send(RPCResponse::visibility_change(
                self.window.visible(),
                prevent_close as u32,
                saved_style.full_screen,
            ))
            .ok();
    }
    fn transmit_window_state_change(&self) {
        if let Some(hwnd) = self.window.handle.hwnd() {
            let web_channel = self.webview.channel.borrow();
            let (web_tx, _) = web_channel
                .as_ref()
                .expect("Cannont obtain communication channel for the Web UI");
            let web_tx_app = web_tx.clone();
            let style = self.saved_window_style.borrow();
            let state = style.clone().get_window_state(hwnd);
            web_tx_app.send(RPCResponse::state_change(state)).ok();
        }
    }
    fn on_init(&self) {
        self.webview.endpoint.set(self.webui_url.clone()).ok();
        self.webview.dev_tools.set(self.dev_tools).ok();
        if let Some(hwnd) = self.window.handle.hwnd() {
            let mut saved_style = self.saved_window_style.borrow_mut();
            saved_style.center_window(hwnd, Self::MIN_WIDTH, Self::MIN_HEIGHT);
        }

        self.tray.tray_show_hide.set_checked(true);

        let player_channel = self.player.channel.borrow();
        let (player_tx, player_rx) = player_channel
            .as_ref()
            .expect("Cannont obtain communication channel for the Player");
        let player_tx = player_tx.clone();
        let player_rx = player_rx.clone();

        let web_channel = self.webview.channel.borrow();
        let (web_tx, web_rx) = web_channel
            .as_ref()
            .expect("Cannont obtain communication channel for the Web UI");
        let web_tx_player = web_tx.clone();
        let web_tx_web = web_tx.clone();
        let web_tx_arg = web_tx.clone();
        let web_rx = web_rx.clone();
        let command_clone = self.command.clone();

        // Single application IPC
        let socket_path = Path::new(
            self.commands_path
                .as_ref()
                .expect("Cannot initialie the single application IPC"),
        );
        if let Ok(mut listener) = PipeServer::bind(socket_path) {
            thread::spawn(move || loop {
                if let Ok(mut stream) = listener.accept() {
                    let mut buf = vec![];
                    stream.read_to_end(&mut buf).ok();
                    if let Ok(s) = str::from_utf8(&buf) {
                        // ['open-media', url]
                        web_tx_arg.send(RPCResponse::open_media(s.to_string())).ok();
                        println!("{}", s);
                    }
                }
            });
        }

        // Read message from player
        thread::spawn(move || loop {
            player_rx
                .iter()
                .map(|msg| web_tx_player.send(msg))
                .for_each(drop);
        }); // thread

        let toggle_fullscreen_sender = self.toggle_fullscreen_notice.sender();
        let quit_sender = self.quit_notice.sender();
        let hide_splash_sender = self.hide_splash_notice.sender();
        let focus_sender = self.focus_notice.sender();
        thread::spawn(move || loop {
            if let Some(msg) = web_rx
                .recv()
                .ok()
                .and_then(|s| serde_json::from_str::<RPCRequest>(&s).ok())
            {
                match msg.get_method() {
                    // The handshake. Here we send some useful data to the WEB UI
                    None if msg.is_handshake() => {
                        web_tx_web.send(RPCResponse::get_handshake()).ok();
                    }
                    Some("win-set-visibility") => toggle_fullscreen_sender.notice(),
                    Some("quit") => quit_sender.notice(),
                    Some("app-ready") => {
                        hide_splash_sender.notice();
                        web_tx_web
                            .send(RPCResponse::visibility_change(true, 1, false))
                            .ok();
                        let command_ref = command_clone.clone();
                        if !command_ref.is_empty() {
                            web_tx_web.send(RPCResponse::open_media(command_ref)).ok();
                        }
                    }
                    Some("app-error") => {
                        hide_splash_sender.notice();
                        if let Some(arg) = msg.get_params() {
                            // TODO: Make this modal dialog
                            eprintln!("Web App Error: {}", arg);
                        }
                    }
                    Some("open-external") => {
                        if let Some(arg) = msg.get_params() {
                            // FIXME: THIS IS NOT SAFE BY ANY MEANS
                            // open::that("calc").ok(); does exactly that
                            let arg = arg.as_str().unwrap_or("");
                            let arg_lc = arg.to_lowercase();
                            if arg_lc.starts_with("http://")
                                || arg_lc.starts_with("https://")
                                || arg_lc.starts_with("rtp://")
                                || arg_lc.starts_with("rtps://")
                                || arg_lc.starts_with("ftp://")
                                || arg_lc.starts_with("ipfs://")
                            {
                                open::that(arg).ok();
                            }
                        }
                    }
                    Some("win-focus") => {
                        focus_sender.notice();
                    }
                    Some(player_command) if player_command.starts_with("mpv-") => {
                        let resp_json = serde_json::to_string(
                            &msg.args.expect("Cannot have method without args"),
                        )
                        .expect("Cannot build response");
                        player_tx.send(resp_json).ok();
                    }
                    Some(unknown) => {
                        eprintln!("Unsupported command {}({:?})", unknown, msg.get_params())
                    }
                    None => {}
                }
            } // recv
        }); // thread
    }
    fn on_min_max(&self, data: &nwg::EventData) {
        let data = data.on_min_max();
        data.set_min_size(Self::MIN_WIDTH, Self::MIN_HEIGHT);
    }
    fn on_paint(&self) {
        if self.splash_screen.visible() {
            self.splash_screen.resize(self.window.size());
        }
    }
    fn on_toggle_fullscreen_notice(&self) {
        if let Some(hwnd) = self.window.handle.hwnd() {
            let mut saved_style = self.saved_window_style.borrow_mut();
            saved_style.toggle_full_screen(hwnd);
            self.tray.tray_topmost.set_enabled(!saved_style.full_screen);
            self.tray
                .tray_topmost
                .set_checked((saved_style.ex_style as u32 & WS_EX_TOPMOST) == WS_EX_TOPMOST);
        }
        self.transmit_window_full_screen_change(true);
    }
    fn on_hide_splash_notice(&self) {
        self.splash_screen.hide();
    }
    fn on_focus_notice(&self) {
        self.window.set_visible(true);
        if let Some(hwnd) = self.window.handle.hwnd() {
            let mut saved_style = self.saved_window_style.borrow_mut();
            saved_style.set_active(hwnd);
        }
    }
    fn on_toggle_topmost(&self) {
        if let Some(hwnd) = self.window.handle.hwnd() {
            let mut saved_style = self.saved_window_style.borrow_mut();
            saved_style.toggle_topmost(hwnd);
            self.tray
                .tray_topmost
                .set_checked((saved_style.ex_style as u32 & WS_EX_TOPMOST) == WS_EX_TOPMOST);
        }
    }
    fn on_show_hide(&self) {
        self.window.set_visible(!self.window.visible());
        self.tray.tray_show_hide.set_checked(self.window.visible());
        self.transmit_window_state_change();
    }
    fn on_quit(&self, data: &nwg::EventData) {
        if let nwg::EventData::OnWindowClose(data) = data {
            data.close(false);
        }
        self.window.set_visible(false);
        self.tray.tray_show_hide.set_checked(self.window.visible());
        self.transmit_window_full_screen_change(false);
        nwg::stop_thread_dispatch();
    }
}
