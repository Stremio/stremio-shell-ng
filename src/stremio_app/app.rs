use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use serde_json;
use std::cell::RefCell;
use std::cmp;
use std::sync::Arc;
use std::thread;
use winapi::shared::windef::HWND__;
use winapi::um::winuser::{
    GetForegroundWindow, GetSystemMetrics, GetWindowLongA, IsIconic, IsZoomed, SetWindowLongA,
    SetWindowPos, GWL_EXSTYLE, GWL_STYLE, HWND_NOTOPMOST, HWND_TOPMOST, SM_CXSCREEN, SM_CYSCREEN,
    SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, WS_CAPTION, WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME,
    WS_EX_STATICEDGE, WS_EX_TOPMOST, WS_EX_WINDOWEDGE, WS_THICKFRAME,
};

use crate::stremio_app::ipc::{RPCRequest, RPCResponse, RPCResponseData, RPCResponseDataTransport};
use crate::stremio_app::stremio_player::Player;
use crate::stremio_app::stremio_wevbiew::WebView;
use crate::stremio_app::systray::SystemTray;

// https://doc.qt.io/qt-5/qt.html#WindowState-enum
bitflags! {
    struct WindowState: u8 {
        const MINIMIZED = 0x01;
        const MAXIMIZED = 0x02;
        const FULL_SCREEN = 0x04;
        const ACTIVE = 0x08;
    }
}

#[derive(Default, Clone)]
pub struct WindowStyle {
    pub full_screen: bool,
    pub pos: (i32, i32),
    pub size: (u32, u32),
    pub style: i32,
    pub ex_style: i32,
}

impl WindowStyle {
    pub fn get_window_state(self, hwnd: *mut HWND__) -> u32 {
        let mut state: WindowState = WindowState::empty();
        if 0 != unsafe { IsIconic(hwnd) } {
            state |= WindowState::MINIMIZED;
        }
        if 0 != unsafe { IsZoomed(hwnd) } {
            state |= WindowState::MAXIMIZED;
        }
        if hwnd == unsafe { GetForegroundWindow() } {
            state |= WindowState::ACTIVE
        }
        if self.full_screen {
            state |= WindowState::FULL_SCREEN;
        }
        state.bits() as u32
    }
}

#[derive(Default, NwgUi)]
pub struct MainWindow {
    pub webui_url: String,
    pub saved_window_style: RefCell<WindowStyle>,
    #[nwg_resource]
    pub embed: nwg::EmbedResource,
    #[nwg_resource(source_embed: Some(&data.embed), source_embed_str: Some("MAINICON"))]
    pub window_icon: nwg::Icon,
    #[nwg_control(icon: Some(&data.window_icon), title: "Stremio", flags: "MAIN_WINDOW|VISIBLE")]
    #[nwg_events( OnWindowClose: [Self::on_quit(SELF, EVT_DATA)], OnInit: [Self::on_init], OnPaint: [Self::on_paint], OnMinMaxInfo: [Self::on_min_max(SELF, EVT_DATA)], OnWindowMaximize: [Self::transmit_window_state_change], OnWindowMinimize: [Self::transmit_window_state_change] )]
    pub window: nwg::Window,
    #[nwg_partial(parent: window)]
    #[nwg_events((tray, MousePressLeftUp): [Self::on_show_hide], (tray_exit, OnMenuItemSelected): [Self::on_quit_notice], (tray_show_hide, OnMenuItemSelected): [Self::on_show_hide], (tray_topmost, OnMenuItemSelected): [Self::on_toggle_topmost]) ]
    pub tray: SystemTray,
    #[nwg_partial(parent: window)]
    pub webview: WebView,
    #[nwg_partial(parent: window)]
    pub player: Player,
    #[nwg_resource(size: Some((300,300)), source_embed: Some(&data.embed), source_embed_str: Some("SPLASHIMAGE"))]
    pub splash_image: nwg::Bitmap,
    #[nwg_control(parent: window, background_color: Some(Self::BG_COLOR))]
    pub splash_frame: nwg::ImageFrame,
    #[nwg_control(parent: splash_frame, background_color: Some(Self::BG_COLOR), bitmap: Some(&data.splash_image))]
    pub splash: nwg::ImageFrame,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_toggle_fullscreen_notice] )]
    pub toggle_fullscreen_notice: nwg::Notice,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_quit_notice] )]
    pub quit_notice: nwg::Notice,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_hide_splash_notice] )]
    pub hide_splash_notice: nwg::Notice,
}

impl MainWindow {
    const BG_COLOR: [u8; 3] = [27, 17, 38];
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
        let small_side = cmp::min(nwg::Monitor::width(), nwg::Monitor::height()) * 70 / 100;
        let dimensions = (
            cmp::max(small_side * 16 / 9, Self::MIN_WIDTH),
            cmp::max(small_side, Self::MIN_HEIGHT),
        );
        let [total_width, total_height] = [nwg::Monitor::width(), nwg::Monitor::height()];
        let x = (total_width - dimensions.0) / 2;
        let y = (total_height - dimensions.1) / 2;
        self.window
            .set_size(dimensions.0 as u32, dimensions.1 as u32);
        self.window.set_position(x, y);

        self.tray.tray_show_hide.set_checked(true);

        let player_channel = self.player.channel.borrow();
        let (player_tx, player_rx) = player_channel
            .as_ref()
            .expect("Cannont obtain communication channel for the Player");
        let player_tx = player_tx.clone();
        let player_rx = Arc::clone(player_rx);

        let web_channel = self.webview.channel.borrow();
        let (web_tx, web_rx) = web_channel
            .as_ref()
            .expect("Cannont obtain communication channel for the Web UI");
        let web_tx_player = web_tx.clone();
        let web_tx_web = web_tx.clone();
        let web_rx = Arc::clone(web_rx);
        // Read message from player
        thread::spawn(move || loop {
            let rx = player_rx.lock().unwrap();
            if let Ok(msg) = rx.recv() {
                web_tx_player
                    .send(RPCResponse::response_message(
                        serde_json::from_str(&msg).ok(),
                    ))
                    .ok();
            } // recv
        }); // thread

        let toggle_fullscreen_sender = self.toggle_fullscreen_notice.sender();
        let quit_sender = self.quit_notice.sender();
        let hide_splash_sender = self.hide_splash_notice.sender();
        thread::spawn(move || loop {
            let rx = web_rx.lock().unwrap();
            if let Ok(msg) = rx.recv() {
                if let Ok(msg) = serde_json::from_str::<RPCRequest>(&msg) {
                    // The handshake. Here we send some useful data to the WEB UI
                    if msg.id == 0 {
                        let resp = RPCResponse {
                            id: 0,
                            object: "transport".to_string(),
                            response_type: 3,
                            data: Some(RPCResponseData {
                                transport: RPCResponseDataTransport {
                                    properties: vec![
                                        vec![],
                                        vec![
                                            "".to_string(),
                                            "shellVersion".to_string(),
                                            "".to_string(),
                                            "5.0.0".to_string(),
                                        ],
                                    ],
                                    signals: vec![],
                                    methods: vec![vec!["onEvent".to_string(), "".to_string()]],
                                },
                            }),
                            ..Default::default()
                        };
                        let resp_json =
                            serde_json::to_string(&resp).expect("Cannot build response");
                        web_tx_web.send(resp_json).ok();
                    } else if let Some(args) = msg.args {
                        if let Some(method) = args.first() {
                            let method = method.as_str().unwrap_or("invalid-method");
                            if method.starts_with("mpv-") {
                                let resp_json =
                                    serde_json::to_string(&args).expect("Cannot build response");
                                player_tx.send(resp_json).ok();
                            } else {
                                match method {
                                    "win-set-visibility" => {
                                        toggle_fullscreen_sender.notice();
                                    }
                                    "quit" => {
                                        quit_sender.notice();
                                    }
                                    "app-ready" => {
                                        hide_splash_sender.notice();
                                        web_tx_web
                                            .send(RPCResponse::visibility_change(true, 1, false))
                                            .ok();
                                    }
                                    "app-error" => {
                                        hide_splash_sender.notice();
                                        if args.len() > 1 {
                                            // TODO: Make this modal dialog
                                            eprintln!(
                                                "Web App Error: {}",
                                                args[1].as_str().unwrap_or("Unknown error")
                                            );
                                        }
                                    }
                                    "open-external" => {
                                        if args.len() > 1 {
                                            if let Some(arg) = args[1].as_str() {
                                                // FIXME: THIS IS NOT SAFE BY ANY MEANS
                                                // open::that("calc").ok(); does exactly that
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
                                    }
                                    _ => eprintln!("Unsupported command {:?}", args),
                                }
                            }
                        }
                    }
                } else {
                    eprintln!("Web UI sent invalid JSON: {:?}", msg);
                }
            } // recv
        }); // thread
    }
    fn on_min_max(&self, data: &nwg::EventData) {
        let data = data.on_min_max();
        data.set_min_size(Self::MIN_WIDTH, Self::MIN_HEIGHT);
    }
    fn on_paint(&self) {
        if self.splash_frame.visible() {
            let (w, h) = self.window.size();
            let s = cmp::min(w, h);
            self.splash_frame.set_size(w, h);
            self.splash.set_size(s, s);
            self.splash.set_position(w as i32 / 2 - s as i32 / 2, 0);
        } else {
            self.transmit_window_state_change();
        }
    }
    fn on_toggle_fullscreen_notice(&self) {
        if let Some(hwnd) = self.window.handle.hwnd() {
            let mut saved_style = self.saved_window_style.borrow_mut();
            if saved_style.full_screen {
                let topmost = if saved_style.ex_style as u32 & WS_EX_TOPMOST == WS_EX_TOPMOST {
                    HWND_TOPMOST
                } else {
                    HWND_NOTOPMOST
                };
                unsafe {
                    SetWindowLongA(hwnd, GWL_STYLE, saved_style.style);
                    SetWindowLongA(hwnd, GWL_EXSTYLE, saved_style.ex_style);
                    SetWindowPos(
                        hwnd,
                        topmost,
                        saved_style.pos.0,
                        saved_style.pos.1,
                        saved_style.size.0 as i32,
                        saved_style.size.1 as i32,
                        SWP_FRAMECHANGED,
                    );
                }
                saved_style.full_screen = false;
                self.tray.tray_topmost.set_enabled(true);
                self.tray.tray_topmost.set_checked(topmost == HWND_TOPMOST);
            } else {
                saved_style.pos = self.window.position();
                saved_style.size = self.window.size();
                unsafe {
                    saved_style.style = GetWindowLongA(hwnd, GWL_STYLE);
                    saved_style.ex_style = GetWindowLongA(hwnd, GWL_EXSTYLE);
                    SetWindowLongA(
                        hwnd,
                        GWL_STYLE,
                        saved_style.style & !(WS_CAPTION as i32 | WS_THICKFRAME as i32),
                    );
                    SetWindowLongA(
                        hwnd,
                        GWL_EXSTYLE,
                        saved_style.ex_style
                            & !(WS_EX_DLGMODALFRAME as i32
                                | WS_EX_WINDOWEDGE as i32
                                | WS_EX_CLIENTEDGE as i32
                                | WS_EX_STATICEDGE as i32),
                    );
                    SetWindowPos(
                        hwnd,
                        HWND_NOTOPMOST,
                        0,
                        0,
                        GetSystemMetrics(SM_CXSCREEN),
                        GetSystemMetrics(SM_CYSCREEN),
                        SWP_FRAMECHANGED,
                    );
                }
                saved_style.full_screen = true;
                self.tray.tray_topmost.set_enabled(false);
            }
        }
        self.transmit_window_full_screen_change(true);
    }
    fn on_quit_notice(&self) {
        nwg::stop_thread_dispatch();
    }
    fn on_hide_splash_notice(&self) {
        self.splash_frame.set_visible(false);
    }
    fn on_toggle_topmost(&self) {
        if let Some(hwnd) = self.window.handle.hwnd() {
            let topmost = if unsafe { GetWindowLongA(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST
                == WS_EX_TOPMOST
            {
                HWND_NOTOPMOST
            } else {
                HWND_TOPMOST
            };
            unsafe {
                SetWindowPos(
                    hwnd,
                    topmost,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_FRAMECHANGED,
                );
            }
            let mut saved_style = self.saved_window_style.borrow_mut();
            saved_style.ex_style = unsafe { GetWindowLongA(hwnd, GWL_EXSTYLE) };
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
    }
}
