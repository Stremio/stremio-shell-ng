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
        let web_tx = web_tx.clone();
        let web_rx = Arc::clone(web_rx);
        thread::spawn(move || {
            loop {
                // Read message from player
                {
                    let rx = player_rx.lock().unwrap();
                    if let Ok(msg) = rx.try_recv() {
                        let resp = RPCResponse {
                            id: 1,
                            object: "transport".to_string(),
                            response_type: 1,
                            args: serde_json::from_str(&msg).ok(),
                            ..Default::default()
                        };
                        let resp_json =
                            serde_json::to_string(&resp).expect("Cannot serialize the response");
                        web_tx.send(resp_json).ok();
                    }
                };

                // read message from WebView
                {
                    let rx = web_rx.lock().unwrap();
                    if let Ok(msg) = rx.try_recv() {
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
                                            methods: vec![vec![
                                                "onEvent".to_string(),
                                                "".to_string(),
                                            ]],
                                        },
                                    }),
                                    ..Default::default()
                                };
                                let resp_json = serde_json::to_string(&resp).unwrap();
                                web_tx.send(resp_json).ok();
                            } else if let Some(args) = msg.args {
                                // TODO: this can panic
                                if let Some(method) = args.first() {
                                    let method = method.as_str().unwrap();
                                    if method.starts_with("mpv-") {
                                        let resp_json = serde_json::to_string(&args).unwrap();
                                        player_tx.send(resp_json).ok();
                                    } else {
                                        eprintln!("Unsupported command {:?}", args)
                                    }
                                }
                            }
                        } else {
                            eprintln!("Web UI sent invalid JSON: {:?}", msg);
                        }
                    } // try_recv
                };
            }
        });
        // // let video_path = "/home/ivo/storage/bbb_sunflower_1080p_30fps_normal.mp4";
        // let video_path = "http://distribution.bbb3d.renderfarming.net/video/mp4/bbb_sunflower_1080p_30fps_normal.mp4";
        // self.player.command(&["loadfile", video_path]);
        // // self.player.set_prop("time-pos", 120.0);
        // self.player.set_prop("speed", 2.0);
        // // self.player.set_prop("pause", true);
        // self.player.command(&["stop"]);
    }
    fn on_quit(&self) {
        nwg::stop_thread_dispatch();
    }
}
