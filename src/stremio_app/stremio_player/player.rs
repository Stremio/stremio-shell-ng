use crate::stremio_app::gpu_video_processing;
use crate::stremio_app::ipc;
use crate::stremio_app::RPCResponse;
use flume::{Receiver, Sender};
use libmpv2::{events::Event, Format, Mpv, SetData};
use native_windows_gui::{self as nwg, PartialUi};
use std::{
    collections::VecDeque,
    convert::TryFrom,
    ffi::CString,
    mem,
    os::raw::c_char,
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use winapi::shared::{
    minwindef::{DWORD, UINT},
    windef::{HMONITOR, HWND},
    winerror::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS},
};
use winapi::um::{
    wingdi::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        QDC_ONLY_ACTIVE_PATHS,
    },
    winnt::LONG,
    winuser::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
    },
};

use crate::stremio_app::stremio_player::{
    CmdVal, InMsg, InMsgArgs, InMsgFn, PlayerEnded, PlayerEvent, PlayerProprChange, PlayerResponse,
    PropKey, PropVal,
};

struct ObserveProperty {
    name: String,
    format: Format,
}

#[derive(Debug, Eq, PartialEq)]
struct SubtitleCommand {
    name: &'static str,
    args: Vec<String>,
}

impl TryFrom<CmdVal> for SubtitleCommand {
    type Error = CmdVal;

    fn try_from(command: CmdVal) -> Result<Self, Self::Error> {
        match command {
            CmdVal::SubAdd(url, title, language) => Ok(Self {
                name: "sub-add",
                args: vec![
                    "sub-add".to_string(),
                    url,
                    "auto".to_string(),
                    title,
                    language,
                ],
            }),
            CmdVal::SubRemove(track_id) => Ok(Self {
                name: "sub-remove",
                args: vec!["sub-remove".to_string(), track_id],
            }),
            command => Err(command),
        }
    }
}

enum SubtitleWorkerRequest {
    Command(SubtitleCommand),
    BeginMediaChange(Sender<()>),
    EndMediaChange,
}

struct InFlightSubtitle {
    reply_id: u64,
    name: &'static str,
}

enum SubtitleWorkerEvent {
    Request(Result<SubtitleWorkerRequest, flume::RecvError>),
    Wake(Result<(), flume::RecvError>),
}

fn start_next_subtitle(
    mpv: &Mpv,
    queue: &mut VecDeque<SubtitleCommand>,
    in_flight: &mut Option<InFlightSubtitle>,
    next_reply_id: &mut u64,
    media_change: bool,
) {
    while !media_change && in_flight.is_none() {
        let Some(command) = queue.pop_front() else {
            return;
        };
        let args = match command
            .args
            .iter()
            .map(|argument| CString::new(argument.as_str()))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(args) => args,
            Err(_) => {
                eprintln!("mpv: invalid {} argument", command.name);
                continue;
            }
        };
        let mut arg_ptrs = args
            .iter()
            .map(|arg| arg.as_ptr())
            .chain(std::iter::once(std::ptr::null::<c_char>()))
            .collect::<Vec<_>>();
        let reply_id = *next_reply_id;
        *next_reply_id = (*next_reply_id).wrapping_add(1).max(1);
        let result = unsafe {
            libmpv2_sys::mpv_command_async(mpv.ctx.as_ptr(), reply_id, arg_ptrs.as_mut_ptr())
        };
        if result < 0 {
            eprintln!(
                "mpv: failed to queue {}: {}",
                command.name,
                libmpv2_sys::mpv_error_str(result)
            );
            continue;
        }
        *in_flight = Some(InFlightSubtitle {
            reply_id,
            name: command.name,
        });
    }
}

fn create_subtitle_thread(
    mut mpv: Mpv,
    request_receiver: Receiver<SubtitleWorkerRequest>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("subtitle-loader".to_string())
        .spawn(move || {
            let (wake_sender, wake_receiver) = flume::bounded(1);
            mpv.set_wakeup_callback(move || {
                let _ = wake_sender.try_send(());
            });

            let mut queue = VecDeque::new();
            let mut in_flight: Option<InFlightSubtitle> = None;
            let mut next_reply_id = 1;
            let mut media_change = false;
            let mut media_change_ack: Option<Sender<()>> = None;

            loop {
                let event = flume::Selector::new()
                    .recv(&request_receiver, SubtitleWorkerEvent::Request)
                    .recv(&wake_receiver, SubtitleWorkerEvent::Wake)
                    .wait();

                match event {
                    SubtitleWorkerEvent::Request(Ok(SubtitleWorkerRequest::Command(command))) => {
                        queue.push_back(command);
                    }
                    SubtitleWorkerEvent::Request(Ok(SubtitleWorkerRequest::BeginMediaChange(
                        acknowledge,
                    ))) => {
                        media_change = true;
                        queue.clear();
                        if let Some(command) = &in_flight {
                            media_change_ack = Some(acknowledge);
                            unsafe {
                                libmpv2_sys::mpv_abort_async_command(
                                    mpv.ctx.as_ptr(),
                                    command.reply_id,
                                );
                            }
                        } else {
                            let _ = acknowledge.send(());
                        }
                    }
                    SubtitleWorkerEvent::Request(Ok(SubtitleWorkerRequest::EndMediaChange)) => {
                        media_change = false;
                    }
                    SubtitleWorkerEvent::Request(Err(_)) | SubtitleWorkerEvent::Wake(Err(_)) => {
                        break;
                    }
                    SubtitleWorkerEvent::Wake(Ok(())) => loop {
                        let event = unsafe { &*libmpv2_sys::mpv_wait_event(mpv.ctx.as_ptr(), 0.0) };
                        match event.event_id {
                            libmpv2_sys::mpv_event_id_MPV_EVENT_NONE => break,
                            libmpv2_sys::mpv_event_id_MPV_EVENT_SHUTDOWN => return,
                            libmpv2_sys::mpv_event_id_MPV_EVENT_COMMAND_REPLY => {
                                let Some(command) = &in_flight else {
                                    continue;
                                };
                                if command.reply_id != event.reply_userdata {
                                    continue;
                                }
                                let command = in_flight.take().expect("missing subtitle command");
                                if event.error < 0 {
                                    eprintln!(
                                        "mpv: {} failed: {}",
                                        command.name,
                                        libmpv2_sys::mpv_error_str(event.error)
                                    );
                                }
                                if let Some(acknowledge) = media_change_ack.take() {
                                    let _ = acknowledge.send(());
                                }
                            }
                            _ => {}
                        }
                    },
                }

                start_next_subtitle(
                    &mpv,
                    &mut queue,
                    &mut in_flight,
                    &mut next_reply_id,
                    media_change,
                );
            }
        })
        .expect("failed to start subtitle worker")
}

#[derive(Debug, Default)]
struct VideoReadyState {
    next_load_id: u64,
    current_load_id: Option<u64>,
    pending_load_ids: VecDeque<u64>,
    active_load_id: Option<u64>,
    file_loaded_id: Option<u64>,
    ready_sent_id: Option<u64>,
}

impl VideoReadyState {
    fn begin_transition(&mut self, loads_file: bool) -> u64 {
        self.next_load_id += 1;
        self.current_load_id = Some(self.next_load_id);
        self.file_loaded_id = None;
        self.ready_sent_id = None;
        if loads_file {
            self.pending_load_ids.push_back(self.next_load_id);
        }
        self.next_load_id
    }

    fn start_file(&mut self) {
        self.active_load_id = self.pending_load_ids.pop_front();
        self.file_loaded_id = None;
    }

    fn file_loaded(&mut self) {
        if self.active_load_id == self.current_load_id {
            self.file_loaded_id = self.active_load_id;
        }
    }

    fn playback_restarted(&mut self) -> Option<u64> {
        let load_id = self.current_load_id?;
        if self.file_loaded_id != Some(load_id) || self.ready_sent_id == Some(load_id) {
            return None;
        }
        self.ready_sent_id = Some(load_id);
        Some(load_id)
    }
}

type SharedVideoReadyState = Arc<Mutex<VideoReadyState>>;

fn video_ready_response(load_id: u64, ready: bool) -> String {
    RPCResponse::response_message(PlayerResponse::video_ready(load_id, ready).to_value())
}

fn send_video_ready(rpc_response_sender: &Sender<String>, load_id: u64, ready: bool) {
    if let Err(error) = rpc_response_sender.send(video_ready_response(load_id, ready)) {
        eprintln!("failed to send video readiness: {error}");
    }
}

#[link(name = "user32")]
extern "system" {
    fn GetDisplayConfigBufferSizes(
        flags: UINT,
        num_path_array_elements: *mut UINT,
        num_mode_info_array_elements: *mut UINT,
    ) -> LONG;
    fn QueryDisplayConfig(
        flags: UINT,
        num_path_array_elements: *mut UINT,
        path_array: *mut DISPLAYCONFIG_PATH_INFO,
        num_mode_info_array_elements: *mut UINT,
        mode_info_array: *mut DISPLAYCONFIG_MODE_INFO,
        current_topology_id: *mut u32,
    ) -> LONG;
    fn DisplayConfigGetDeviceInfo(request_packet: *mut DISPLAYCONFIG_DEVICE_INFO_HEADER) -> LONG;
}

const DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2: u32 = 15;
const DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR: u32 = 2;

#[repr(C)]
struct DisplayconfigGetAdvancedColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
    color_encoding: u32,
    bits_per_color_channel: u32,
    active_color_mode: u32,
}

fn with_gpu_next_fallback(vo: String) -> String {
    let mut outputs = vo
        .split(',')
        .filter(|output| !output.is_empty())
        .map(String::from)
        .collect::<Vec<String>>();

    let has_gpu_next = outputs.iter().any(|output| output == "gpu-next");
    let has_gpu = outputs.iter().any(|output| output == "gpu");

    if outputs.is_empty() {
        outputs.push("gpu-next".to_string());
        outputs.push("gpu".to_string());
    } else if has_gpu_next && !has_gpu {
        outputs.push("gpu".to_string());
    } else if has_gpu && !has_gpu_next {
        outputs.push("gpu-next".to_string());
    }

    format!("{},", outputs.join(","))
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DisplayOutputMode {
    Hdr,
    Sdr,
    Auto,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct DisplayOutputState {
    mode: DisplayOutputMode,
    scale_percent: u32,
}

#[derive(Default)]
pub struct Player {
    pub channel: ipc::Channel,
}

impl PartialUi for Player {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        // @TODO replace with `&mut self`?
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        // @TODO replace all `expect`s with proper error handling?

        let window_handle = parent
            .expect("no parent window")
            .into()
            .hwnd()
            .expect("cannot obtain window handle");

        let (in_msg_sender, in_msg_receiver) = flume::unbounded();
        let (rpc_response_sender, rpc_response_receiver) = flume::unbounded();
        let (observe_property_sender, observe_property_receiver) = flume::unbounded();
        data.channel = ipc::Channel::new(Some((in_msg_sender, rpc_response_receiver)));
        let video_ready_state = Arc::new(Mutex::new(VideoReadyState::default()));

        let mpv = Arc::new(create_mpv(window_handle));
        let mpv_event_client = mpv
            .create_client(None)
            .expect("cannot create MPV event client");
        let _event_thread = create_event_thread(
            mpv_event_client,
            observe_property_receiver,
            rpc_response_sender.clone(),
            Arc::clone(&video_ready_state),
        );
        let gpu_video_processing = Arc::new(AtomicBool::new(false));
        let _display_thread = create_display_output_thread(
            Arc::clone(&mpv),
            window_handle as isize,
            Arc::clone(&gpu_video_processing),
        );
        let _message_thread = create_message_thread(
            mpv,
            window_handle as isize,
            gpu_video_processing,
            observe_property_sender,
            in_msg_receiver,
            rpc_response_sender,
            video_ready_state,
        );
        // @TODO implement a mechanism to stop threads on `Player` drop if needed

        Ok(())
    }
}

fn create_mpv(window_handle: HWND) -> Mpv {
    let mpv = Mpv::with_initializer(|initializer| {
        macro_rules! set_property {
            ($name:literal, $value:expr) => {
                initializer
                    .set_property($name, $value)
                    .expect(concat!("failed to set ", $name));
            };
        }
        set_property!("wid", window_handle as i64);
        set_property!("title", "Stremio");
        set_property!("audio-client-name", "Stremio");
        set_property!("terminal", "yes");
        #[cfg(debug_assertions)]
        set_property!("msg-level", "all=no,cplayer=debug");
        #[cfg(not(debug_assertions))]
        set_property!("msg-level", "all=no");
        set_property!("quiet", "yes");
        set_property!("hwdec", "auto");
        // `%23%` escapes the 23-byte HTTP status list as one mpv option value.
        set_property!(
            "stream-lavf-o",
            "reconnect=1,reconnect_streamed=1,reconnect_on_network_error=1,reconnect_on_http_error=%23%408,429,500,502,503,504,reconnect_delay_max=15"
        );
        // gpu-next: libplacebo VO with modern HDR tone-mapping; gpu, is the fallback.
        set_property!("vo", "gpu-next,gpu,");
        let mut options = vec![
            ("gpu-context", "d3d11"),
            ("d3d11-output-format", "auto"),
            ("d3d11-output-csp", "auto"),
            ("target-colorspace-hint", "auto"),
            ("target-colorspace-hint-mode", "target"),
            ("tone-mapping", "bt.2390"),
            ("dither-depth", "auto"),
        ];
        if gpu_video_processing::unified_memory_architecture() {
            options.push(("profile", "fast"));
        } else {
            options.extend([
                ("deband", "yes"),
                ("scale", "spline36"),
                ("cscale", "spline36"),
            ]);
        }
        for (name, value) in options {
            if let Err(error) = initializer.set_property(name, value) {
                eprintln!("mpv: cannot set {name}={value}: {error:?}");
            }
        }
        Ok(())
    });
    mpv.expect("cannot build MPV")
}

fn create_display_output_thread(
    mpv: Arc<Mpv>,
    window_handle: isize,
    gpu_video_processing: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut last_state = None;

        loop {
            let state = current_display_output_state(&mpv, window_handle as HWND);
            let gpu = gpu_video_processing.load(Ordering::Relaxed);
            if last_state != Some((state, gpu)) {
                apply_display_output_mode(&mpv, state, gpu);
                last_state = Some((state, gpu));
            }
            thread::sleep(Duration::from_millis(500));
        }
    })
}

fn current_display_output_state(mpv: &Mpv, window_handle: HWND) -> DisplayOutputState {
    DisplayOutputState {
        mode: current_display_output_mode(window_handle),
        scale_percent: current_video_filter_scale(mpv, window_handle),
    }
}

fn current_display_output_mode(window_handle: HWND) -> DisplayOutputMode {
    let monitor = unsafe { MonitorFromWindow(window_handle, MONITOR_DEFAULTTONEAREST) };
    match monitor_hdr_active(monitor) {
        Some(true) => DisplayOutputMode::Hdr,
        Some(false) => DisplayOutputMode::Sdr,
        None => DisplayOutputMode::Auto,
    }
}

fn current_video_filter_scale(mpv: &Mpv, window_handle: HWND) -> u32 {
    let Some(video_height) = current_video_height(mpv) else {
        return 100;
    };
    let Some(display_height) = current_monitor_height(window_handle) else {
        return 100;
    };
    if video_height <= 0.0 || display_height <= video_height {
        return 100;
    }

    ((display_height / video_height).min(4.0) * 100.0).round() as u32
}

fn current_video_height(mpv: &Mpv) -> Option<f64> {
    let video_params = mpv.get_property::<String>("video-params").ok()?;
    let video_params = serde_json::from_str::<serde_json::Value>(&video_params).ok()?;
    video_params.get("h").and_then(serde_json::Value::as_f64)
}

fn current_monitor_height(window_handle: HWND) -> Option<f64> {
    let monitor = unsafe { MonitorFromWindow(window_handle, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return None;
    }

    let mut monitor_info: MONITORINFO = unsafe { mem::zeroed() };
    monitor_info.cbSize = mem::size_of::<MONITORINFO>() as DWORD;
    if unsafe { GetMonitorInfoW(monitor, &mut monitor_info) } == 0 {
        return None;
    }

    Some((monitor_info.rcMonitor.bottom - monitor_info.rcMonitor.top) as f64)
}

fn apply_display_output_mode(mpv: &Mpv, state: DisplayOutputState, gpu_video_processing: bool) {
    // Target colorspace follows the display so native HDR still passes through; the RTX
    // HDR filter is the opt-in part and only makes sense on an HDR display.
    let vf = if gpu_video_processing {
        let scale = state.scale_percent as f64 / 100.0;
        let mut vf = format!("d3d11vpp=scaling-mode=nvidia:scale={scale:.2}");
        if state.mode == DisplayOutputMode::Hdr {
            vf.push_str(":format=x2bgr10:nvidia-true-hdr");
        }
        vf
    } else {
        String::new()
    };
    let color = match state.mode {
        DisplayOutputMode::Hdr | DisplayOutputMode::Auto => [
            ("d3d11-output-csp", "auto"),
            ("target-colorspace-hint", "auto"),
            ("target-trc", "auto"),
            ("target-prim", "auto"),
        ],
        DisplayOutputMode::Sdr => [
            ("d3d11-output-csp", "srgb"),
            ("target-colorspace-hint", "yes"),
            ("target-trc", "srgb"),
            ("target-prim", "bt.709"),
        ],
    };

    for (name, value) in std::iter::once(("vf", vf.as_str())).chain(color) {
        if let Err(error) = mpv.set_property(name, value) {
            eprintln!("mpv: cannot set {name}={value}: {error:?}");
        }
    }
}

fn monitor_hdr_active(monitor: HMONITOR) -> Option<bool> {
    if monitor.is_null() {
        return None;
    }

    let device_name = monitor_device_name(monitor)?;
    for path in active_display_paths()? {
        let Some(source_name) = display_source_name(&path) else {
            continue;
        };
        if source_name.viewGdiDeviceName != device_name {
            continue;
        }

        return display_hdr_active(&path);
    }

    None
}

fn monitor_device_name(monitor: HMONITOR) -> Option<[u16; 32]> {
    let mut monitor_info: MONITORINFOEXW = unsafe { mem::zeroed() };
    monitor_info.cbSize = mem::size_of::<MONITORINFOEXW>() as DWORD;

    let result =
        unsafe { GetMonitorInfoW(monitor, &mut monitor_info as *mut _ as *mut MONITORINFO) };
    if result == 0 {
        None
    } else {
        Some(monitor_info.szDevice)
    }
}

fn active_display_paths() -> Option<Vec<DISPLAYCONFIG_PATH_INFO>> {
    for _ in 0..3 {
        let mut path_count = 0;
        let mut mode_count = 0;
        let buffer_status = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        };
        if buffer_status != ERROR_SUCCESS as LONG {
            return None;
        }

        let mut paths =
            vec![unsafe { mem::zeroed::<DISPLAYCONFIG_PATH_INFO>() }; path_count as usize];
        let mut modes =
            vec![unsafe { mem::zeroed::<DISPLAYCONFIG_MODE_INFO>() }; mode_count as usize];
        let query_status = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                ptr::null_mut(),
            )
        };

        if query_status == ERROR_SUCCESS as LONG {
            paths.truncate(path_count as usize);
            return Some(paths);
        }
        if query_status != ERROR_INSUFFICIENT_BUFFER as LONG {
            return None;
        }
    }

    None
}

fn display_source_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<DISPLAYCONFIG_SOURCE_DEVICE_NAME> {
    let mut source_name: DISPLAYCONFIG_SOURCE_DEVICE_NAME = unsafe { mem::zeroed() };
    source_name.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
        _type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        size: mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
        adapterId: path.sourceInfo.adapterId,
        id: path.sourceInfo.id,
    };

    let status = unsafe { DisplayConfigGetDeviceInfo(&mut source_name.header) };
    if status == ERROR_SUCCESS as LONG {
        Some(source_name)
    } else {
        None
    }
}

fn display_hdr_active(path: &DISPLAYCONFIG_PATH_INFO) -> Option<bool> {
    let mut color_info: DisplayconfigGetAdvancedColorInfo2 = unsafe { mem::zeroed() };
    color_info.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
        _type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2,
        size: mem::size_of::<DisplayconfigGetAdvancedColorInfo2>() as u32,
        adapterId: path.targetInfo.adapterId,
        id: path.targetInfo.id,
    };

    let status = unsafe { DisplayConfigGetDeviceInfo(&mut color_info.header) };
    if status == ERROR_SUCCESS as LONG {
        Some(color_info.active_color_mode == DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR)
    } else {
        None
    }
}

fn create_event_thread(
    mpv_event_client: Mpv,
    observe_property_receiver: Receiver<ObserveProperty>,
    rpc_response_sender: Sender<String>,
    video_ready_state: SharedVideoReadyState,
) -> JoinHandle<()> {
    thread::spawn(move || {
        mpv_event_client
            .disable_deprecated_events()
            .expect("failed to disable deprecated MPV events");

        // -- Event handler loop --

        loop {
            for ObserveProperty { name, format } in observe_property_receiver.drain() {
                mpv_event_client
                    .observe_property(&name, format, 0)
                    .expect("failed to observer MPV property");
            }

            let event = match mpv_event_client.wait_event(0.1) {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    eprintln!("Event errored: {error:?}");
                    continue;
                }
                // dummy event received (may be created on a wake up call or on timeout)
                None => continue,
            };

            if matches!(&event, Event::StartFile) {
                video_ready_state
                    .lock()
                    .expect("cannot lock video readiness state")
                    .start_file();
                continue;
            }

            if matches!(&event, Event::FileLoaded) {
                video_ready_state
                    .lock()
                    .expect("cannot lock video readiness state")
                    .file_loaded();
                continue;
            }

            if matches!(&event, Event::PlaybackRestart) {
                let load_id = video_ready_state
                    .lock()
                    .expect("cannot lock video readiness state")
                    .playback_restarted();
                if let Some(load_id) = load_id {
                    send_video_ready(&rpc_response_sender, load_id, true);
                }
                continue;
            }

            // even if you don't do anything with the events, it is still necessary to empty the event loop
            let player_response = match event {
                Event::PropertyChange { name, change, .. } => PlayerResponse(
                    "mpv-prop-change",
                    PlayerEvent::PropChange(PlayerProprChange::from_name_value(
                        name.to_string(),
                        change,
                    )),
                ),
                Event::EndFile(reason) => PlayerResponse(
                    "mpv-event-ended",
                    PlayerEvent::End(PlayerEnded::from_end_reason(reason)),
                ),
                Event::Shutdown => {
                    break;
                }
                _ => continue,
            };

            if let Err(error) =
                rpc_response_sender.send(RPCResponse::response_message(player_response.to_value()))
            {
                eprintln!("failed to send RPCResponse: {error}");
                break;
            }
        }
    })
}

fn create_message_thread(
    mpv: Arc<Mpv>,
    window_handle: isize,
    gpu_video_processing: Arc<AtomicBool>,
    observe_property_sender: Sender<ObserveProperty>,
    in_msg_receiver: Receiver<String>,
    rpc_response_sender: Sender<String>,
    video_ready_state: SharedVideoReadyState,
) -> JoinHandle<()> {
    thread::spawn(move || {
        // -- Helpers --
        let subtitle_mpv = mpv
            .create_client(Some("subtitle-loader"))
            .expect("cannot create MPV subtitle client");
        let (subtitle_request_sender, subtitle_request_receiver) = flume::unbounded();
        let _subtitle_thread = create_subtitle_thread(subtitle_mpv, subtitle_request_receiver);

        let observe_property = |name: String, format: Format| {
            if let Err(error) = observe_property_sender.send(ObserveProperty { name, format }) {
                eprintln!("cannot send ObserveProperty: {error}");
            }
        };

        let send_command = |cmd: CmdVal| {
            let cmd = match SubtitleCommand::try_from(cmd) {
                Ok(command) => {
                    if subtitle_request_sender
                        .send(SubtitleWorkerRequest::Command(command))
                        .is_err()
                    {
                        eprintln!("mpv: subtitle worker stopped");
                    }
                    return;
                }
                Err(command) => command,
            };
            let media_transition = cmd.media_transition();
            let (acknowledge, acknowledged) = flume::bounded(1);
            if subtitle_request_sender
                .send(SubtitleWorkerRequest::BeginMediaChange(acknowledge))
                .is_err()
                || acknowledged.recv().is_err()
            {
                eprintln!("mpv: subtitle worker stopped before media change");
                return;
            }
            let parts: Vec<String> = cmd.into();
            if let Some((name, args)) = parts.split_first() {
                if let Some(loads_file) = media_transition {
                    let load_id = video_ready_state
                        .lock()
                        .expect("cannot lock video readiness state")
                        .begin_transition(loads_file);
                    send_video_ready(&rpc_response_sender, load_id, false);
                }
                let args = args.iter().map(String::as_str).collect::<Vec<_>>();
                if let Err(error) = mpv.command(name, &args) {
                    eprintln!("failed to execute MPV command: '{error:#}'")
                }
            }
            if subtitle_request_sender
                .send(SubtitleWorkerRequest::EndMediaChange)
                .is_err()
            {
                eprintln!("mpv: subtitle worker stopped after media change");
            }
        };

        fn set_property(name: impl ToString, value: impl SetData, mpv: &Mpv) {
            if let Err(error) = mpv.set_property(&name.to_string(), value) {
                eprintln!("cannot set MPV property: '{error:#}'")
            }
        }

        // -- InMsg handler loop --

        for msg in in_msg_receiver.iter() {
            let in_msg: InMsg = match serde_json::from_str(&msg) {
                Ok(in_msg) => in_msg,
                Err(error) => {
                    eprintln!("cannot parse InMsg:{:?} {error:#}", msg);
                    continue;
                }
            };

            match in_msg {
                InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Bool(prop))) => {
                    observe_property(prop.to_string(), Format::Flag);
                }
                InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Int(prop))) => {
                    observe_property(prop.to_string(), Format::Int64);
                }
                InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Fp(prop))) => {
                    observe_property(prop.to_string(), Format::Double);
                }
                InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Str(prop))) => {
                    observe_property(prop.to_string(), Format::String);
                }
                InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(name, PropVal::Bool(value))) => {
                    set_property(name, value, &mpv);
                }
                InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(name, PropVal::Num(value))) => {
                    set_property(name, value, &mpv);
                }
                InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(name, PropVal::Str(value))) => {
                    let is_vo = name.to_string() == "vo";
                    let value = if is_vo {
                        with_gpu_next_fallback(value)
                    } else {
                        value
                    };
                    set_property(name, value, &mpv);
                    // vo reinit reverts color props to defaults; re-assert for the current display.
                    if is_vo {
                        apply_display_output_mode(
                            &mpv,
                            current_display_output_state(&mpv, window_handle as HWND),
                            gpu_video_processing.load(Ordering::Relaxed),
                        );
                    }
                }
                InMsg(InMsgFn::MpvSetGpuVideoProcessing, InMsgArgs::Flag(enabled)) => {
                    gpu_video_processing.store(enabled, Ordering::Relaxed);
                    apply_display_output_mode(
                        &mpv,
                        current_display_output_state(&mpv, window_handle as HWND),
                        enabled,
                    );
                }
                InMsg(InMsgFn::MpvCommand, InMsgArgs::Cmd(cmd)) => {
                    send_command(cmd);
                }
                msg => {
                    eprintln!("MPV unsupported message: '{msg:?}'");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::VideoReadyState;

    #[test]
    fn ready_follows_file_loaded_restart_once() {
        let mut state = VideoReadyState::default();
        let load_id = state.begin_transition(true);
        state.start_file();
        assert_eq!(state.playback_restarted(), None);
        state.file_loaded();
        assert_eq!(state.playback_restarted(), Some(load_id));
        assert_eq!(state.playback_restarted(), None);
    }

    #[test]
    fn new_transition_ignores_previous_file_events() {
        let mut state = VideoReadyState::default();
        state.begin_transition(true);
        state.start_file();

        let current_load = state.begin_transition(true);
        state.file_loaded();
        assert_eq!(state.playback_restarted(), None);

        state.start_file();
        state.file_loaded();
        assert_eq!(state.playback_restarted(), Some(current_load));
    }

    #[test]
    fn stop_never_becomes_ready() {
        let mut state = VideoReadyState::default();
        state.begin_transition(false);
        state.file_loaded();
        assert_eq!(state.playback_restarted(), None);
    }
}
