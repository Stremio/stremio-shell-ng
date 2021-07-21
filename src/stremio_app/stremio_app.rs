use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use serde::{Deserialize, Serialize};
use serde_json;
use std::cmp;
use std::sync::Arc;
use std::thread;

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
//////////////////////////////////////////

#[derive(Default, NwgUi)]
pub struct MainWindow {
    pub webui_url: String,
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
    pub splash_image: nwg::Icon,
    #[nwg_control(icon: Some(&data.splash_image))]
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
                        let resp_json = serde_json::to_string(&resp).unwrap();
                        web_tx_web.send(resp_json).ok();
                    } else if let Some(args) = msg.args {
                        // TODO: this can panic
                        if let Some(method) = args.first() {
                            let method = method.as_str().unwrap();
                            if method.starts_with("mpv-") {
                                let resp_json = serde_json::to_string(&args).unwrap();
                                player_tx.send(resp_json).ok();
                            } else {
                                match method {
                                    "toggle-fullscreen" => {
                                        toggle_fullscreen_sender.notice();
                                    }
                                    "quit" => {
                                        quit_sender.notice();
                                    }
                                    "app-ready" => {
                                        hide_splash_sender.notice();
                                    }
                                    "app-error" => {
                                        hide_splash_sender.notice();
                                        if args.len() > 1 {
                                            // TODO: Make this modal dialog
                                            eprintln!("Web App Error: {}", args[1].as_str().unwrap_or("Unknown error"));
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
        let (w, h) = self.window.size();
        self.splash.set_size(w, h);
    }
    fn on_toggle_fullscreen_notice(&self) {
        println!("full screen toggle requested");
    }
    fn on_quit_notice(&self) {
        self.on_quit();
    }
    fn on_hide_splash_notice(&self) {
        self.splash.set_visible(false);
    }
    fn on_quit(&self) {
        nwg::stop_thread_dispatch();
    }
}
