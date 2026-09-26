use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use rand::Rng;
use serde_json;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    io::Read,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{self, Command},
    str,
    sync::{Arc, Mutex},
    thread, time,
};
use url::Url;
use winapi::{
    shared::windef::RECT,
    um::{
        winbase::CREATE_BREAKAWAY_FROM_JOB,
        winuser::{
            GetWindowLongA, SetWindowPos, GWL_STYLE, SWP_NOACTIVATE, SWP_NOZORDER, WM_DPICHANGED,
            WS_CAPTION, WS_EX_TOPMOST,
        },
    },
};

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
use super::stremio_server::{ServerEvent, StremioServer};

const WEBUI_RETRY_DELAYS_SECS: [u64; 2] = [2, 5];
const WEBUI_READY_TIMEOUT_SECS: u64 = 30;
const WEBUI_RETRY_READY_TIMEOUT_SECS: u64 = 15;
const WEBUI_UNREACHABLE: &str = "Stremio couldn't load its interface. \
    Check your internet connection, VPN or firewall, then click Retry.";

pub enum WebUiEvent {
    Ready,
    LoadFailed(String),
    Timeout(u64),
    Retry(u64),
}

type WebUiEvents = Arc<Mutex<VecDeque<WebUiEvent>>>;

fn send_webui_event(events: &WebUiEvents, notice: &nwg::NoticeSender, event: WebUiEvent) {
    events.lock().unwrap().push_back(event);
    notice.notice();
}

pub enum OpenRequest {
    Input(String),
    Ready,
    Reset,
}

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
    pub requested_interface_scale: Arc<Mutex<Option<u64>>>,
    pub saved_window_style: RefCell<WindowStyle>,
    pub open_media_sender: RefCell<Option<flume::Sender<OpenRequest>>>,
    pub local_server_url: Arc<Mutex<Option<String>>>,
    pub webui_events: WebUiEvents,
    pub webui_ready: Cell<bool>,
    pub webui_retries: Cell<usize>,
    pub webui_generation: Cell<u64>,
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
    #[nwg_events((notice, OnNotice): [Self::on_server_notice])]
    pub server: StremioServer,
    #[nwg_partial(parent: window)]
    pub player: Player,
    #[nwg_partial(parent: window)]
    pub webview: WebView,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_toggle_fullscreen_notice] )]
    pub toggle_fullscreen_notice: nwg::Notice,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_set_interface_scale_notice] )]
    pub set_interface_scale_notice: nwg::Notice,
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
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_webui_notice])]
    pub webui_notice: nwg::Notice,
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
        self.webview.dev_tools.set(self.dev_tools).ok();
        if let Some(hwnd) = self.window.handle.hwnd() {
            if let Ok(mut saved_style) = self.saved_window_style.try_borrow_mut() {
                saved_style.set_title_bar_color(hwnd);
                nwg::bind_raw_event_handler(&self.window.handle, 0x10001, |hwnd, msg, _w, l| {
                    if msg != WM_DPICHANGED
                        || unsafe { GetWindowLongA(hwnd, GWL_STYLE) } as u32 & WS_CAPTION == 0
                    {
                        return None;
                    }
                    let rect = unsafe { &*(l as *const RECT) };
                    unsafe {
                        SetWindowPos(
                            hwnd,
                            std::ptr::null_mut(),
                            rect.left,
                            rect.top,
                            rect.right - rect.left,
                            rect.bottom - rect.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                    Some(0)
                })
                .ok();
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
        let web_tx_open = web_tx.clone();
        let web_tx_upd = web_tx.clone();
        let web_rx = web_rx.clone();

        let (updater_tx, updater_rx) = flume::unbounded::<String>();
        let updater_tx_web = updater_tx.clone();

        let (open_sender, open_receiver) = flume::unbounded();
        let open_sender_web = open_sender.clone();
        *self.open_media_sender.borrow_mut() = Some(open_sender.clone());
        let command = self.command.clone();
        thread::spawn(move || {
            let mut ready = false;
            let mut pending = (!command.is_empty()).then_some(command);
            for request in open_receiver {
                match request {
                    OpenRequest::Input(input) => pending = Some(input),
                    OpenRequest::Ready => ready = true,
                    OpenRequest::Reset => ready = false,
                }
                if ready {
                    if let Some(input) = pending.take() {
                        let message = super::open_media::message(&input);
                        web_tx_open
                            .send(RPCResponse::response_message(Some(message)))
                            .ok();
                    }
                }
            }
        });

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
                        if !s.is_empty() {
                            open_sender.send(OpenRequest::Input(s.to_string())).ok();
                        }
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
        let set_interface_scale_sender = self.set_interface_scale_notice.sender();
        let quit_sender = self.quit_notice.sender();
        let hide_splash_sender = self.hide_splash_notice.sender();
        let focus_sender = self.focus_notice.sender();
        let autoupdater_setup_mutex = self.autoupdater_setup_file.clone();

        let discord_rpc = DiscordRpc::new(web_tx.clone());
        let requested_fullscreen = self.requested_fullscreen.clone();
        let requested_cache_directory = self.requested_cache_directory.clone();
        let cache_directory_sender = self.cache_directory_notice.sender();
        let local_server_url = self.local_server_url.clone();
        let requested_interface_scale = self.requested_interface_scale.clone();
        let webui_events = self.webui_events.clone();
        let webui_sender = self.webui_notice.sender();

        thread::spawn(move || loop {
            if let Some(msg) = web_rx
                .recv()
                .ok()
                .and_then(|s| serde_json::from_str::<RPCRequest>(&s).ok())
            {
                let local_server_url = local_server_url.lock().unwrap().clone();
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
                    Some("win-set-interface-scale") => {
                        if let Some(scale) = msg
                            .get_params()
                            .and_then(|params| params.get("scale"))
                            .and_then(|value| value.as_u64())
                            .filter(|scale| (75..=175).contains(scale))
                        {
                            *requested_interface_scale.lock().unwrap() = Some(scale);
                            set_interface_scale_sender.notice();
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
                        send_webui_event(&webui_events, &webui_sender, WebUiEvent::Ready);
                        hide_splash_sender.notice();
                        web_tx_web
                            .send(RPCResponse::visibility_change(true, 1, false))
                            .ok();
                        updater_tx_web
                            .send("check_for_update".to_owned())
                            .expect("Failed to send value to updater channel");

                        open_sender_web.send(OpenRequest::Ready).ok();
                    }
                    Some("app-load-error") => {
                        if let Some(arg) = msg.get_params() {
                            let detail = arg.as_str().unwrap_or_default().to_owned();
                            send_webui_event(
                                &webui_events,
                                &webui_sender,
                                WebUiEvent::LoadFailed(detail),
                            );
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
                    Some("play-external") => {
                        if let Some(arg) = msg.get_params().and_then(|value| value.as_str()) {
                            if let Err(error) = crate::stremio_app::external_player::play(arg) {
                                eprintln!("External player request failed: {error}");
                            }
                        }
                    }
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
        if self.server.development() {
            self.load_webui(None);
        } else {
            self.server.start();
        }
    }
    fn load_webui(&self, server_url: Option<&str>) {
        *self.local_server_url.lock().unwrap() = server_url
            .filter(|server_url| {
                Url::parse(server_url).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https")
                        && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
                })
            })
            .map(str::to_owned);
        if let Some(sender) = self.open_media_sender.borrow().as_ref() {
            sender.send(OpenRequest::Reset).ok();
        }
        let endpoint = if self.webui_url.trim_end_matches('/') == WEB_ENDPOINT.trim_end_matches('/')
        {
            server_url
                .map(web_endpoint_with_streaming_server)
                .unwrap_or_else(|| self.webui_url.clone())
        } else {
            self.webui_url.clone()
        };
        self.webui_ready.set(false);
        self.webui_retries.set(0);
        if let Err(error) = self.webview.navigate(endpoint) {
            self.splash_screen.hide();
            nwg::modal_error_message(
                &self.window,
                "Cannot load Stremio Web UI",
                &error.to_string(),
            );
        } else {
            self.start_webui_deadline();
        }
    }
    fn start_webui_deadline(&self) {
        let generation = self.webui_generation.get() + 1;
        self.webui_generation.set(generation);
        let timeout = if self.webui_retries.get() > 0 {
            WEBUI_RETRY_READY_TIMEOUT_SECS
        } else {
            WEBUI_READY_TIMEOUT_SECS
        };
        let events = self.webui_events.clone();
        let sender = self.webui_notice.sender();
        thread::spawn(move || {
            thread::sleep(time::Duration::from_secs(timeout));
            send_webui_event(&events, &sender, WebUiEvent::Timeout(generation));
        });
    }
    fn retry_webui_silently(&self) -> bool {
        let Some(&delay) = WEBUI_RETRY_DELAYS_SECS.get(self.webui_retries.get()) else {
            return false;
        };
        self.webui_retries.set(self.webui_retries.get() + 1);
        let generation = self.webui_generation.get() + 1;
        self.webui_generation.set(generation);
        let events = self.webui_events.clone();
        let sender = self.webui_notice.sender();
        thread::spawn(move || {
            thread::sleep(time::Duration::from_secs(delay));
            send_webui_event(&events, &sender, WebUiEvent::Retry(generation));
        });
        true
    }
    fn retry_webui(&self) {
        match self.webview.retry() {
            Ok(()) => self.start_webui_deadline(),
            Err(error) => self.show_webui_error(&error.to_string()),
        }
    }
    fn show_webui_error(&self, details: &str) {
        self.splash_screen.hide();
        self.on_show();
        let choice = nwg::modal_message(
            &self.window,
            &nwg::MessageParams {
                title: "Stremio",
                content: details,
                buttons: nwg::MessageButtons::RetryCancel,
                icons: nwg::MessageIcons::Error,
            },
        );
        if choice == nwg::MessageChoice::Retry {
            if !self.no_splash {
                self.splash_screen.show();
            }
            self.retry_webui();
        } else {
            self.on_exit();
        }
    }
    fn on_webui_notice(&self) {
        let events: Vec<_> = self.webui_events.lock().unwrap().drain(..).collect();
        for event in events {
            let current = self.webui_generation.get();
            match event {
                WebUiEvent::Ready => {
                    self.webui_ready.set(true);
                    self.webui_retries.set(0);
                }
                _ if self.webui_ready.get() => {}
                WebUiEvent::LoadFailed(detail) => {
                    eprintln!("Web UI load error: {detail}");
                    if !self.retry_webui_silently() {
                        self.show_webui_error(&format!("{WEBUI_UNREACHABLE}\n\n{detail}"));
                    }
                }
                WebUiEvent::Timeout(generation) if generation == current => {
                    eprintln!("Web UI startup timed out");
                    if self.webui_retries.get() > 0 || !self.retry_webui_silently() {
                        self.show_webui_error(WEBUI_UNREACHABLE);
                    }
                }
                WebUiEvent::Retry(generation) if generation == current => {
                    eprintln!("Retrying the web UI (attempt {})", self.webui_retries.get());
                    self.retry_webui();
                }
                WebUiEvent::Timeout(_) | WebUiEvent::Retry(_) => {}
            }
        }
    }
    fn on_server_notice(&self) {
        for event in self.server.events() {
            match event {
                ServerEvent::Ready(endpoint) => self.load_webui(Some(&endpoint)),
                ServerEvent::Failed(details) => {
                    *self.local_server_url.lock().unwrap() = None;
                    if let Some(sender) = self.open_media_sender.borrow().as_ref() {
                        sender.send(OpenRequest::Reset).ok();
                    }
                    self.splash_screen.hide();
                    self.on_show();
                    let content = format!(
                        "Stremio's local streaming server is unavailable.\n\n{details}\n\nChoose Retry to start the server again, or Cancel to exit Stremio."
                    );
                    let choice = nwg::modal_message(
                        &self.window,
                        &nwg::MessageParams {
                            title: "Stremio server",
                            content: &content,
                            buttons: nwg::MessageButtons::RetryCancel,
                            icons: nwg::MessageIcons::Error,
                        },
                    );
                    if choice == nwg::MessageChoice::Retry {
                        if !self.no_splash {
                            self.splash_screen.show();
                        }
                        self.server.start();
                    } else {
                        self.on_exit();
                    }
                }
            }
        }
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
    fn on_set_interface_scale_notice(&self) {
        let scale = self.requested_interface_scale.lock().unwrap().take();
        if let Some(scale) = scale {
            self.webview.set_interface_scale(scale);
        }
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
