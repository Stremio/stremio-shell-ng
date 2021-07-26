use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use serde::{Deserialize, Serialize};
use serde_json::{self, json};
use std::cell::RefCell;
use std::cmp;
use std::sync::Arc;
use std::thread;
use winapi::um::winuser::{
    GetSystemMetrics, GetWindowLongA, SetWindowLongA, GWL_EXSTYLE, GWL_STYLE, SM_CXSCREEN,
    SM_CYSCREEN, WS_CAPTION, WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME, WS_EX_STATICEDGE,
    WS_EX_WINDOWEDGE, WS_THICKFRAME,
};

use crate::stremio_app::stremio_player::Player;
use crate::stremio_app::stremio_wevbiew::WebView;

//////////////////////////////////////////
#[derive(Serialize, Deserialize, Debug, Clone)]
struct RPCRequest {
    id: u64,
    args: Option<Vec<serde_json::Value>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct RPCResponseDataTransport {
    properties: Vec<Vec<String>>,
    signals: Vec<String>,
    methods: Vec<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct RPCResponseData {
    transport: RPCResponseDataTransport,
}

#[derive(Default, Serialize, Deserialize, Debug, Clone)]
struct RPCResponse {
    id: u64,
    object: String,
    #[serde(rename = "type")]
    response_type: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<RPCResponseData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<serde_json::Value>,
}

impl RPCResponse {
    fn visibility_change(visible: bool, visibility: u32, is_full_screen: bool) -> String {
        let resp = RPCResponse {
            id: 1,
            object: "transport".to_string(),
            response_type: 1,
            args: Some(json!(["win-visibility-changed" ,{
                "visible": visible,
                "visibility": visibility,
                "isFullscreen": is_full_screen
            }])),
            ..Default::default()
        };
        serde_json::to_string(&resp).expect("Cannot build response")
    }
}
//////////////////////////////////////////
#[derive(Default)]
pub struct WindowStyle {
    pub full_screen: bool,
    pub pos: (i32, i32),
    pub size: (u32, u32),
    pub style: i32,
    pub ex_style: i32,
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
    #[nwg_events( OnWindowClose: [Self::on_quit], OnInit: [Self::on_init], OnPaint: [Self::on_paint], OnMinMaxInfo: [Self::on_min_max(SELF, EVT_DATA)] )]
    pub window: nwg::Window,
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
                let resp = RPCResponse {
                    id: 1,
                    object: "transport".to_string(),
                    response_type: 1,
                    args: serde_json::from_str(&msg).ok(),
                    ..Default::default()
                };
                let resp_json =
                    serde_json::to_string(&resp).expect("Cannot serialize the response");
                web_tx_player.send(resp_json).ok();
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
        }
    }
    fn on_toggle_fullscreen_notice(&self) {
        if let Some(hwnd) = self.window.handle.hwnd() {
            let mut saved_style = self.saved_window_style.borrow_mut();
            if saved_style.full_screen {
                unsafe {
                    SetWindowLongA(hwnd, GWL_STYLE, saved_style.style);
                    SetWindowLongA(hwnd, GWL_EXSTYLE, saved_style.ex_style);
                }
                self.window
                    .set_position(saved_style.pos.0, saved_style.pos.1);
                self.window.set_size(saved_style.size.0, saved_style.size.1);
                saved_style.full_screen = false;
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
                }
                self.window.set_position(0, 0);
                self.window
                    .set_size(unsafe { GetSystemMetrics(SM_CXSCREEN) as u32 }, unsafe {
                        GetSystemMetrics(SM_CYSCREEN) as u32
                    });
                saved_style.full_screen = true;
            }
            let web_channel = self.webview.channel.borrow();
            let (web_tx, _) = web_channel
                .as_ref()
                .expect("Cannont obtain communication channel for the Web UI");
            let web_tx_app = web_tx.clone();
            web_tx_app
                .send(RPCResponse::visibility_change(
                    true,
                    1,
                    saved_style.full_screen,
                ))
                .ok();
        }
    }
    fn on_quit_notice(&self) {
        self.on_quit();
    }
    fn on_hide_splash_notice(&self) {
        self.splash_frame.set_visible(false);
    }
    fn on_quit(&self) {
        nwg::stop_thread_dispatch();
    }
}
