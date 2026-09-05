use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use rand::Rng;
use serde_json;
use std::{
    cell::RefCell,
    io::Read,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{self, Command},
    str,
    sync::{Arc, Mutex},
    thread, time,
};
use url::Url;
use winapi::um::{winbase::CREATE_BREAKAWAY_FROM_JOB, winuser::WS_EX_TOPMOST};

use crate::stremio_app::{
    constants::{
        safe_url, web_endpoint_with_streaming_server, APP_NAME, UPDATE_ENDPOINT, UPDATE_INTERVAL,
        WEB_ENDPOINT, WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH,
    },
    ipc::{CacheDirectoryRequest, RPCRequest, RPCResponse},
    splash::SplashImage,
    stremio_player::Player,
    stremio_wevbiew::WebView,
    systray::SystemTray,
    updater,
    window_helper::WindowStyle,
    window_settings::WindowSettings,
    PipeServer,
};

use super::discord::DiscordRpc;
use super::stremio_server::StremioServer;

#[derive(Default, NwgUi)]
pub struct MainWindow {
    pub command: String,
    pub commands_path: Option<String>,
    pub webui_url: String,
    pub no_splash: bool,
    pub dev_tools: bool,
    pub start_hidden: bool,
    pub autoupdater_endpoint: Option<Url>,
    pub force_update: bool,
    pub release_candidate: bool,
    pub autoupdater_setup_file: Arc<Mutex<Option<PathBuf>>>,
    pub requested_fullscreen: Arc<Mutex<Option<bool>>>,
    pub requested_cache_directory: Arc<Mutex<Option<CacheDirectoryRequest>>>,
    pub saved_window_style: RefCell<WindowStyle>,
    #[nwg_resource]
    pub embed: nwg::EmbedResource,
    #[nwg_resource(source_embed: Some(&data.embed), source_embed_str: Some("MAINICON"))]
    pub window_icon: nwg::Icon,
    #[nwg_resource(title: "Choose cache folder", action: nwg::FileDialogAction::OpenDirectory)]
    pub cache_directory_picker: nwg::FileDialog,
    #[nwg_control(icon: Some(&data.window_icon), title: APP_NAME, flags: "MAIN_WINDOW")]
    #[nwg_events(
        OnWindowClose: [Self::on_quit(SELF, EVT_DATA)],
        OnInit: [Self::on_init],
        OnPaint: [Self::on_paint],
        OnMinMaxInfo: [Self::on_min_max(SELF, EVT_DATA)],
        OnWindowMinimize: [Self::transmit_window_state_change],
        OnWindowMaximize: [Self::on_window_state_changed],
        OnWindowFocus: [Self::transmit_window_state_change],
        OnResizeEnd: [Self::save_window_settings],
    )]
    pub window: nwg::Window,
    #[nwg_partial(parent: window)]
    #[nwg_events(
        (tray, MousePressLeftUp): [Self::on_show],
        (tray_exit, OnMenuItemSelected): [Self::on_exit],
        (tray_show_hide, OnMenuItemSelected): [Self::on_show_hide],
        (tray_topmost, OnMenuItemSelected): [Self::on_toggle_topmost],
    )]
    pub tray: SystemTray,
    #[nwg_partial(parent: window)]
    pub splash_screen: SplashImage,
    #[nwg_partial(parent: window)]
    pub server: StremioServer,
    #[nwg_partial(parent: window)]
    pub player: Player,
    #[nwg_partial(parent: window)]
    pub webview: WebView,
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
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_cache_directory_notice])]
    pub cache_directory_notice: nwg::Notice,
}

impl MainWindow {
    fn transmit_window_visibility_change(&self) {
        if let (Ok(web_channel), Ok(style)) = (
            self.webview.channel.try_borrow(),
            self.saved_window_style.try_borrow(),
        ) {
            let (web_tx, _) = web_channel
                .as_ref()
                .expect("Cannont obtain communication channel for the Web UI");
            let web_tx_app = web_tx.clone();
            web_tx_app
                .send(RPCResponse::visibility_change(
                    self.window.visible(),
                    style.full_screen as u32,
                    style.full_screen,
                ))
                .ok();
        } else {
            eprintln!("Cannot obtain communication channel or window style");
        }
    }
    fn transmit_window_state_change(&self) {
        if let (Some(hwnd), Ok(web_channel), Ok(style)) = (
            self.window.handle.hwnd(),
            self.webview.channel.try_borrow(),
            self.saved_window_style.try_borrow(),
        ) {
            let state = style.clone().get_window_state(hwnd);
            drop(style);
            let (web_tx, _) = web_channel
                .as_ref()
                .expect("Cannont obtain communication channel for the Web UI");
            let web_tx_app = web_tx.clone();
            web_tx_app.send(RPCResponse::state_change(state)).ok();
        } else {
            eprintln!("Cannot obtain window handle or communication channel");
        }
    }
    fn on_init(&self) {
        let webui_url =
            if self.webui_url.trim_end_matches('/') == WEB_ENDPOINT.trim_end_matches('/') {
                self.server
                    .server_url()
                    .map(|server_url| web_endpoint_with_streaming_server(&server_url))
                    .unwrap_or_else(|| self.webui_url.clone())
            } else {
                self.webui_url.clone()
            };
        self.webview.endpoint.set(webui_url).ok();
        self.webview.dev_tools.set(self.dev_tools).ok();
        if let Some(hwnd) = self.window.handle.hwnd() {
            if let Ok(mut saved_style) = self.saved_window_style.try_borrow_mut() {
                saved_style.set_title_bar_color(hwnd);
                if let Some(window_settings) = WindowSettings::load() {
                    saved_style
                        .restore_window_placement(hwnd, window_settings.to_window_placement());
                } else {
                    saved_style.center_window(hwnd, WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT);
                }
            }
        }

        self.window.set_visible(!self.start_hidden);
        self.tray.tray_show_hide.set_checked(!self.start_hidden);
        if self.no_splash {
            self.splash_screen.hide();
        }

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
        let web_tx_upd = web_tx.clone();
        let web_rx = web_rx.clone();

        let (updater_tx, updater_rx) = flume::unbounded::<String>();
        let updater_tx_web = updater_tx.clone();

        let command_clone = self.command.clone();

        // Single application IPC
        let socket_path = Path::new(
            self.commands_path
                .as_ref()
                .expect("Cannot initialie the single application IPC"),
        );

        let autoupdater_endpoint = self.autoupdater_endpoint.clone();
        let force_update = self.force_update;
        let release_candidate = self.release_candidate;
        let autoupdater_setup_file = self.autoupdater_setup_file.clone();

        thread::spawn(move || {
            loop {
                if let Ok(msg) = updater_rx.recv() {
                    if msg == "check_for_update" {
                        break;
                    }
                }
            }

            loop {
                let current_version = env!("CARGO_PKG_VERSION")
                    .parse()
                    .expect("Should always be valid");

                let updater_endpoint = if let Some(ref endpoint) = autoupdater_endpoint {
                    endpoint.clone()
                } else {
                    let mut rng = rand::thread_rng();
                    let index = rng.gen_range(0..UPDATE_ENDPOINT.len());
                    let mut url = Url::parse(UPDATE_ENDPOINT[index]).unwrap();
                    url.query_pairs_mut().append_pair("arch", env!("ARCH"));
                    if release_candidate {
                        url.query_pairs_mut().append_pair("rc", "true");
                    }
                    url
                };

                let updater =
                    updater::Updater::new(current_version, &updater_endpoint, force_update);
                match updater.autoupdate() {
                    Ok(Some(update)) => {
                        println!("New version ready to install v{}", update.version);
                        let mut autoupdater_setup_file = autoupdater_setup_file.lock().unwrap();
                        *autoupdater_setup_file = Some(update.file.clone());
                        web_tx_upd.send(RPCResponse::update_available()).ok();
                    }
                    Ok(None) => println!("No new updates found"),
                    Err(e) => eprintln!("Failed to fetch updates: {e}"),
                }

                thread::sleep(time::Duration::from_secs(UPDATE_INTERVAL));
            }
        }); // thread

        if let Ok(mut listener) = PipeServer::bind(socket_path) {
            let focus_sender = self.focus_notice.sender();
            thread::spawn(move || loop {
                if let Ok(mut stream) = listener.accept() {
                    let mut buf = vec![];
                    stream.read_to_end(&mut buf).ok();
                    if let Ok(s) = str::from_utf8(&buf) {
                        focus_sender.notice();
                        // ['open-media', url]
                        web_tx_arg.send(RPCResponse::open_media(s.to_string())).ok();
                        println!("{s}");
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
        let autoupdater_setup_mutex = self.autoupdater_setup_file.clone();

        let discord_rpc = DiscordRpc::new(web_tx.clone());
        let requested_fullscreen = self.requested_fullscreen.clone();
        let requested_cache_directory = self.requested_cache_directory.clone();
        let cache_directory_sender = self.cache_directory_notice.sender();
        let local_server_url = self.server.server_url().filter(|server_url| {
            Url::parse(server_url).is_ok_and(|url| {
                matches!(url.scheme(), "http" | "https")
                    && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
            })
        });

        thread::spawn(move || loop {
            if let Some(msg) = web_rx
                .recv()
                .ok()
                .and_then(|s| serde_json::from_str::<RPCRequest>(&s).ok())
            {
                match msg.get_method() {
                    // The handshake. Here we send some useful data to the WEB UI
                    None if msg.is_handshake() => {
                        web_tx_web
                            .send(RPCResponse::get_handshake(local_server_url.as_deref()))
                            .ok();
                    }
                    Some("pick-cache-directory") => {
                        if let Some(request) = msg.get_params().and_then(|params| {
                            serde_json::from_value::<CacheDirectoryRequest>(params.clone()).ok()
                        }) {
                            let mut pending = requested_cache_directory.lock().unwrap();
                            let error = if local_server_url.as_deref()
                                != Some(request.server_url.as_str())
                            {
                                Some("The folder picker is only available for the shell's local server")
                            } else if pending.is_some() {
                                Some("A folder picker is already open")
                            } else {
                                None
                            };
                            if let Some(error) = error {
                                web_tx_web
                                    .send(RPCResponse::cache_directory_selected(
                                        request.request_id,
                                        Err(error.to_owned()),
                                    ))
                                    .ok();
                            } else {
                                *pending = Some(request);
                                cache_directory_sender.notice();
                            }
                        }
                    }
                    Some("win-set-visibility") => {
                        if let Some(fullscreen) = msg
                            .get_params()
                            .and_then(|params| params.get("fullscreen"))
                            .and_then(|value| value.as_bool())
                        {
                            *requested_fullscreen.lock().unwrap() = Some(fullscreen);
                            toggle_fullscreen_sender.notice();
                        }
                    }
                    Some("quit") => quit_sender.notice(),
                    Some("app-ready") => {
                        hide_splash_sender.notice();
                        web_tx_web
                            .send(RPCResponse::visibility_change(true, 1, false))
                            .ok();
                        updater_tx_web
                            .send("check_for_update".to_owned())
                            .expect("Failed to send value to updater channel");

                        let command_ref = command_clone.clone();
                        if !command_ref.is_empty() {
                            web_tx_web.send(RPCResponse::open_media(command_ref)).ok();
                        }
                    }
                    Some("app-error") => {
                        hide_splash_sender.notice();
                        if let Some(arg) = msg.get_params() {
                            // TODO: Make this modal dialog
                            eprintln!("Web App Error: {arg}");
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
                                if let Some(url) = safe_url(arg) {
                                    open::that(url).ok();
                                }
                            }
                        }
                    }
                    // play-external is temporary disabled due to security concerns
                    // Some("play-external") => {
                    //     if let Some(arg) = msg.get_params() {
                    //         let arg = arg.as_str().unwrap_or("");
                    //         let arg_lc = arg.to_lowercase();
                    //         const ALLOWED_SCHEMES: &[&str] = &["mpv://", "vlc://", "potplayer://"];
                    //         let allowed = ALLOWED_SCHEMES.iter().any(|s| arg_lc.starts_with(s));
                    //         if !arg.is_empty() && allowed {
                    //             if let Some(stream_url) =
                    //                 arg_lc.starts_with("mpv://").then(|| &arg[6..])
                    //             {
                    //                 // `--` ends mpv's option parsing; the stream URL can't smuggle flags.
                    //                 let mpv_paths: Vec<String> = vec![
                    //                     std::env::var("ProgramFiles")
                    //                         .ok()
                    //                         .map(|v| format!("{v}\\mpv\\mpv.exe")),
                    //                     std::env::var("ProgramFiles(x86)")
                    //                         .ok()
                    //                         .map(|v| format!("{v}\\mpv\\mpv.exe")),
                    //                     std::env::var("LOCALAPPDATA")
                    //                         .ok()
                    //                         .map(|v| format!("{v}\\Programs\\mpv\\mpv.exe")),
                    //                     std::env::var("LOCALAPPDATA")
                    //                         .ok()
                    //                         .map(|v| format!("{v}\\mpv\\mpv.exe")),
                    //                     Some("mpv.exe".to_string()),
                    //                 ]
                    //                 .into_iter()
                    //                 .flatten()
                    //                 .collect();
                    //                 for path in &mpv_paths {
                    //                     if Command::new(path)
                    //                         .arg("--")
                    //                         .arg(stream_url)
                    //                         .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
                    //                         .spawn()
                    //                         .is_ok()
                    //                     {
                    //                         break;
                    //                     }
                    //                 }
                    //             } else {
                    //                 open::that(arg).ok();
                    //             }
                    //         }
                    //     }
                    // }
                    Some("win-focus") => {
                        focus_sender.notice();
                    }
                    Some("autoupdater-notif-clicked") => {
                        // We've shown the "Update Available" notification
                        // and the user clicked on "Restart And Update"
                        let autoupdater_setup_file =
                            autoupdater_setup_mutex.lock().unwrap().clone();
                        match autoupdater_setup_file {
                            Some(file_path) => {
                                println!("Running the setup at {file_path:?}");

                                let command = Command::new(file_path)
                                    .args([
                                        "/SILENT",
                                        "/NOCANCEL",
                                        "/FORCECLOSEAPPLICATIONS",
                                        "/TASKS=runapp",
                                    ])
                                    .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
                                    .stdin(process::Stdio::null())
                                    .stdout(process::Stdio::null())
                                    .stderr(process::Stdio::null())
                                    .spawn();

                                match command {
                                    Ok(process) => {
                                        println!("Updater started. (PID {:?})", process.id());
                                        quit_sender.notice();
                                    }
                                    Err(err) => eprintln!("Updater couldn't be started: {err}"),
                                };
                            }
                            _ => {
                                println!("Cannot obtain the setup file path");
                            }
                        }
                    }
                    Some("discord-connect") => {
                        if let Err(e) = discord_rpc.connect() {
                            eprintln!("Discord connect error: {}", e);
                            web_tx_web.send(RPCResponse::discord_status(false)).ok();
                        }
                    }
                    Some("discord-disconnect") => {
                        if let Err(e) = discord_rpc.disconnect() {
                            eprintln!("Discord disconnect error: {}", e);
                        }
                        web_tx_web.send(RPCResponse::discord_status(false)).ok();
                    }
                    Some("discord-set-activity") => {
                        if let Some(params) = msg.get_params() {
                            let state = params.get("state").and_then(|v| v.as_str()).unwrap_or("");
                            let details =
                                params.get("details").and_then(|v| v.as_str()).unwrap_or("");
                            let image = params.get("image").and_then(|v| v.as_str());
                            let start_timestamp =
                                params.get("startTimestamp").and_then(|v| v.as_i64());
                            let end_timestamp = params.get("endTimestamp").and_then(|v| v.as_i64());

                            if let Err(e) = discord_rpc.set_activity(
                                state,
                                details,
                                image,
                                start_timestamp,
                                end_timestamp,
                            ) {
                                eprintln!("Discord set activity error: {}", e);
                            }
                        }
                    }
                    Some("discord-clear-activity") => {
                        if let Err(e) = discord_rpc.clear_activity() {
                            eprintln!("Discord clear activity error: {}", e);
                        }
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
        data.set_min_size(WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT);
    }
    fn on_paint(&self) {
        if !self.splash_screen.visible() {
            self.webview.fit_to_window(self.window.handle.hwnd());
        }
    }
    fn on_window_state_changed(&self) {
        self.save_window_settings();
        self.transmit_window_state_change();
    }
    fn save_window_settings(&self) {
        if self
            .saved_window_style
            .try_borrow()
            .map(|style| style.full_screen)
            .unwrap_or(false)
        {
            return;
        }
        if let Some(hwnd) = self.window.handle.hwnd() {
            if let Err(err) = WindowSettings::save(hwnd) {
                eprintln!("Cannot save window settings: {err}");
            }
        }
    }
    fn on_toggle_fullscreen_notice(&self) {
        if let Some(hwnd) = self.window.handle.hwnd() {
            if let Ok(mut saved_style) = self.saved_window_style.try_borrow_mut() {
                let target = self
                    .requested_fullscreen
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap_or(!saved_style.full_screen);
                saved_style.set_full_screen(hwnd, target);
                self.tray.tray_topmost.set_enabled(!saved_style.full_screen);
                self.tray
                    .tray_topmost
                    .set_checked((saved_style.ex_style as u32 & WS_EX_TOPMOST) == WS_EX_TOPMOST);
            }
        }
        self.transmit_window_visibility_change();
    }
    fn on_hide_splash_notice(&self) {
        self.splash_screen.hide();
    }
    fn on_cache_directory_notice(&self) {
        let request = self.requested_cache_directory.lock().unwrap().clone();
        let Some(request) = request else { return };
        if let Some(directory) = &request.directory {
            // An unplugged drive must not prevent choosing a different folder.
            self.cache_directory_picker
                .set_default_folder(directory)
                .ok();
        }
        // Show runs a nested message loop; keep the pending request, but no locks or borrows.
        let result = if self.cache_directory_picker.run(Some(&self.window)) {
            self.cache_directory_picker
                .get_selected_item()
                .map_err(|error| error.to_string())
                .and_then(|path| {
                    path.into_string()
                        .map(Some)
                        .map_err(|_| "The selected folder path is not valid Unicode".to_owned())
                })
        } else {
            Ok(None)
        };
        self.requested_cache_directory.lock().unwrap().take();
        if let Some((web_tx, _)) = self.webview.channel.borrow().as_ref() {
            web_tx
                .send(RPCResponse::cache_directory_selected(
                    request.request_id,
                    result,
                ))
                .ok();
        }
    }
    fn on_focus_notice(&self) {
        self.window.set_visible(true);
        if let Some(hwnd) = self.window.handle.hwnd() {
            if let Ok(mut saved_style) = self.saved_window_style.try_borrow_mut() {
                saved_style.set_active(hwnd);
            }
        }
    }
    fn on_toggle_topmost(&self) {
        if let Some(hwnd) = self.window.handle.hwnd() {
            if let Ok(mut saved_style) = self.saved_window_style.try_borrow_mut() {
                saved_style.toggle_topmost(hwnd);
                self.tray
                    .tray_topmost
                    .set_checked((saved_style.ex_style as u32 & WS_EX_TOPMOST) == WS_EX_TOPMOST);
            }
        }
    }
    fn on_show(&self) {
        self.window.set_visible(true);
        if let (Some(hwnd), Ok(mut saved_style)) = (
            self.window.handle.hwnd(),
            self.saved_window_style.try_borrow_mut(),
        ) {
            if saved_style.is_window_minimized(hwnd) {
                self.window.restore();
            }
            saved_style.set_active(hwnd);
        }
        self.tray.tray_show_hide.set_checked(self.window.visible());
        self.transmit_window_state_change();
        self.transmit_window_visibility_change();
    }
    fn on_show_hide(&self) {
        if self.window.visible() {
            self.window.set_visible(false);
            self.tray.tray_show_hide.set_checked(self.window.visible());
            self.transmit_window_state_change();
            self.transmit_window_visibility_change();
        } else {
            self.on_show();
        }
    }
    fn on_quit(&self, data: &nwg::EventData) {
        if let nwg::EventData::OnWindowClose(data) = data {
            data.close(false);
        }
        self.save_window_settings();
        self.window.set_visible(false);
        self.tray.tray_show_hide.set_checked(self.window.visible());
        self.transmit_window_visibility_change();
    }
    fn on_exit(&self) {
        self.save_window_settings();
        nwg::stop_thread_dispatch();
    }
}
