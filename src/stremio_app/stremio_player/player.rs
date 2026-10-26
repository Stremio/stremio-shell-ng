use crate::stremio_app::gpu_video_processing;
use crate::stremio_app::ipc;
use crate::stremio_app::robust::{
    join_with_timeout, lock, panic_message, report_error, OrReport, RestartPolicy, SERVICES,
};
use crate::stremio_app::RPCResponse;
use flume::{Receiver, Sender};
use libmpv2::{
    events::{Event, PropertyData},
    mpv_end_file_reason, Format, Mpv, SetData,
};
use native_windows_gui::{self as nwg, PartialUi};
use std::{
    collections::VecDeque,
    convert::TryFrom,
    ffi::CString,
    mem,
    os::raw::c_char,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
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

/// `Some(n)` when a JSON number is a whole number that fits an int64.
fn integral_value(value: f64) -> Option<i64> {
    (value.is_finite() && value.fract() == 0.0 && value.abs() < 9.0e15).then_some(value as i64)
}

#[cfg(test)]
mod integral_value_tests {
    use super::integral_value;

    #[test]
    fn whole_numbers_become_int64() {
        assert_eq!(integral_value(2.0), Some(2));
        assert_eq!(integral_value(-1.0), Some(-1));
    }

    #[test]
    fn fractions_and_non_finite_stay_double() {
        assert_eq!(integral_value(1.5), None);
        assert_eq!(integral_value(f64::NAN), None);
        assert_eq!(integral_value(f64::INFINITY), None);
    }
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
        // SAFETY: `mpv.ctx` est un client mpv valide tant que `mpv` est emprunté ; `arg_ptrs` est un tableau terminé par un pointeur nul dont les chaînes (`args`) vivent jusqu'à la fin de l'appel, et mpv copie les arguments.
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
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("subtitle-loader".to_string())
        .spawn(move || {
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| {
                subtitle_worker(&mut mpv, request_receiver)
            })) {
                report_error(
                    "lecteur: fil des sous-titres arrêté par une panique",
                    &panic_message(payload.as_ref()),
                );
            }
        })
}

fn subtitle_worker(mpv: &mut Mpv, request_receiver: Receiver<SubtitleWorkerRequest>) {
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
                    // SAFETY: `ctx` est le pointeur valide du client mpv que ce fil possède ; `mpv_abort_async_command`
                    // accepte tout `reply_id` (un identifiant inconnu est ignoré) et peut être appelé depuis n'importe quel fil.
                    unsafe {
                        libmpv2_sys::mpv_abort_async_command(mpv.ctx.as_ptr(), command.reply_id);
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
                // SAFETY: `ctx` est valide (client possédé par ce fil) ; mpv_wait_event renvoie toujours un événement
                // non nul, valable jusqu'au prochain appel de mpv_wait_event sur ce client, et il n'y en a qu'un par tour.
                let event = unsafe { &*libmpv2_sys::mpv_wait_event(mpv.ctx.as_ptr(), 0.0) };
                match event.event_id {
                    libmpv2_sys::mpv_event_id_MPV_EVENT_NONE => break,
                    libmpv2_sys::mpv_event_id_MPV_EVENT_SHUTDOWN => return,
                    libmpv2_sys::mpv_event_id_MPV_EVENT_COMMAND_REPLY => {
                        if in_flight
                            .as_ref()
                            .is_none_or(|command| command.reply_id != event.reply_userdata)
                        {
                            continue;
                        }
                        let Some(command) = in_flight.take() else {
                            continue;
                        };
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
            mpv,
            &mut queue,
            &mut in_flight,
            &mut next_reply_id,
            media_change,
        );
    }
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

    /// Après une relance de mpv : les chargements en attente de l'ancien mpv ne viendront plus. Les numéros restent
    /// croissants pour que l'interface ne confonde pas deux chargements.
    fn reset_for_restart(&mut self) {
        self.current_load_id = None;
        self.pending_load_ids.clear();
        self.active_load_id = None;
        self.file_loaded_id = None;
        self.ready_sent_id = None;
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

/// Au plus `MAX_RESTARTS` relances de mpv sur `RESTART_WINDOW`, attente de 250 ms, 500 ms, 1 s.
const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(60);
const RESTART_BASE_DELAY: Duration = Duration::from_millis(250);
/// Temps laissé aux fils d'une instance de mpv pour s'arrêter.
const GENERATION_STOP_TIMEOUT: Duration = Duration::from_secs(2);
/// Identifiant de l'observation interne de `time-pos` (l'interface utilise 0).
const POSITION_OBSERVER_ID: u64 = u64::MAX;
/// En dessous de cette position, la reprise repart du début.
const MIN_RESUME_SECONDS: f64 = 1.0;
/// Pas d'attente des fils de l'instance : réactivité à l'arrêt.
const THREAD_POLL: Duration = Duration::from_millis(100);

/// Ce qu'il faut rejouer sur un mpv relancé : réglages, propriétés observées, média en cours et sa position.
#[derive(Debug, Default)]
struct ResumeState {
    properties: Vec<(PropKey, PropVal)>,
    observed: Vec<PropKey>,
    load: Option<CmdVal>,
    position: Option<f64>,
}

impl ResumeState {
    fn record_message(&mut self, message: &InMsg) {
        match message {
            InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(key, value)) => {
                match self.properties.iter_mut().find(|(known, _)| known == key) {
                    Some(entry) => entry.1 = value.clone(),
                    None => self.properties.push((key.clone(), value.clone())),
                }
            }
            InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(key)) => {
                if !self.observed.contains(key) {
                    self.observed.push(key.clone());
                }
            }
            InMsg(InMsgFn::MpvCommand, InMsgArgs::Cmd(command)) => match command {
                CmdVal::Loadfile(_) | CmdVal::LoadfileAt(_, _) => {
                    self.load = Some(command.clone());
                    self.position = None;
                }
                CmdVal::Stop => self.clear_media(),
                CmdVal::SubAdd(..) | CmdVal::SubRemove(_) => {}
            },
            _ => {}
        }
    }

    /// Plus de média à reprendre (arrêt demandé, ou fin de fichier).
    fn clear_media(&mut self) {
        self.load = None;
        self.position = None;
    }

    fn record_position(&mut self, seconds: f64) {
        if self.load.is_some() && seconds.is_finite() && seconds >= 0.0 {
            self.position = Some(seconds);
        }
    }

    /// Commande qui recharge le média à la dernière position connue (ou tel quel si elle est trop proche du début).
    fn resume_command(&self) -> Option<CmdVal> {
        let (CmdVal::Loadfile(url) | CmdVal::LoadfileAt(url, _)) = self.load.as_ref()? else {
            return None;
        };
        match self.position {
            Some(seconds) if seconds >= MIN_RESUME_SECONDS => Some(CmdVal::LoadfileAt(
                url.clone(),
                format!("start=+{seconds:.3}"),
            )),
            _ => self.load.clone(),
        }
    }

    /// Messages à traiter avant tout autre sur un mpv relancé : réglages, observations, puis rechargement du média.
    fn replay_messages(&self) -> Vec<InMsg> {
        let mut messages: Vec<InMsg> = self
            .properties
            .iter()
            .map(|(key, value)| {
                InMsg(
                    InMsgFn::MpvSetProp,
                    InMsgArgs::StProp(key.clone(), value.clone()),
                )
            })
            .collect();
        messages.extend(
            self.observed
                .iter()
                .map(|key| InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(key.clone()))),
        );
        if let Some(command) = self.resume_command() {
            messages.push(InMsg(InMsgFn::MpvCommand, InMsgArgs::Cmd(command)));
        }
        messages
    }
}

/// Ce qui survit aux relances de mpv.
#[derive(Clone)]
struct PlayerContext {
    window_handle: isize,
    in_msg_receiver: Receiver<String>,
    rpc_response_sender: Sender<String>,
    video_ready_state: SharedVideoReadyState,
    gpu_video_processing: Arc<AtomicBool>,
    resume: Arc<Mutex<ResumeState>>,
    /// Les fils d'une instance y signalent un arrêt inattendu de mpv.
    died: Sender<String>,
}

fn signal_death(ctx: &PlayerContext, stop: &AtomicBool, cause: &str) {
    if !stop.load(Ordering::SeqCst) {
        let _ = ctx.died.send(cause.to_owned());
    }
}

/// Fil d'une instance de mpv : une panique est journalisée et signalée comme un arrêt inattendu.
fn spawn_guarded(
    name: &'static str,
    ctx: &PlayerContext,
    stop: &Arc<AtomicBool>,
    body: impl FnOnce() + Send + 'static,
) -> Result<JoinHandle<()>, String> {
    let ctx = ctx.clone();
    let stop = Arc::clone(stop);
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            if let Err(payload) = catch_unwind(AssertUnwindSafe(body)) {
                report_error(
                    &format!("lecteur: fil {name} arrêté par une panique"),
                    &panic_message(payload.as_ref()),
                );
                signal_death(&ctx, &stop, name);
            }
        })
        .map_err(|error| format!("fil {name} non démarré: {error}"))
}

/// Une instance de mpv et ses fils (événements, affichage, messages de l'interface).
struct Generation {
    stop: Arc<AtomicBool>,
    mpv: Arc<Mpv>,
    threads: Vec<(&'static str, JoinHandle<()>)>,
}

impl Generation {
    /// Crée mpv et démarre ses fils ; `preamble` est traité avant tout message de l'interface.
    fn start(ctx: &PlayerContext, preamble: Vec<InMsg>) -> Result<Self, String> {
        let mpv = create_mpv(ctx.window_handle as HWND)
            .map_err(|error| format!("mpv: création impossible: {error}"))?;
        let mut generation = Generation {
            stop: Arc::new(AtomicBool::new(false)),
            mpv: Arc::new(mpv),
            threads: Vec::new(),
        };
        match generation.spawn_threads(ctx, preamble) {
            Ok(()) => Ok(generation),
            Err(error) => {
                generation.stop();
                Err(error)
            }
        }
    }

    fn spawn_threads(&mut self, ctx: &PlayerContext, preamble: Vec<InMsg>) -> Result<(), String> {
        let event_client = self
            .mpv
            .create_client(None)
            .map_err(|error| format!("mpv: client d'événements impossible: {error}"))?;
        let subtitle_client = self
            .mpv
            .create_client(None)
            .map_err(|error| format!("mpv: client de sous-titres impossible: {error}"))?;
        let (observe_property_sender, observe_property_receiver) = flume::unbounded();

        let event_ctx = ctx.clone();
        let event_stop = Arc::clone(&self.stop);
        let handle = spawn_guarded("mpv-events", ctx, &self.stop, move || {
            run_event_loop(
                event_client,
                observe_property_receiver,
                event_ctx,
                event_stop,
            )
        })?;
        self.threads.push(("mpv-events", handle));

        let display_mpv = Arc::clone(&self.mpv);
        let display_stop = Arc::clone(&self.stop);
        let window_handle = ctx.window_handle;
        let gpu_video_processing = Arc::clone(&ctx.gpu_video_processing);
        let handle = spawn_guarded("mpv-display", ctx, &self.stop, move || {
            run_display_loop(
                display_mpv,
                window_handle,
                gpu_video_processing,
                display_stop,
            )
        })?;
        self.threads.push(("mpv-display", handle));

        let message_mpv = Arc::clone(&self.mpv);
        let message_ctx = ctx.clone();
        let message_stop = Arc::clone(&self.stop);
        let handle = spawn_guarded("mpv-messages", ctx, &self.stop, move || {
            run_message_loop(
                message_mpv,
                subtitle_client,
                message_ctx,
                message_stop,
                observe_property_sender,
                preamble,
            )
        })?;
        self.threads.push(("mpv-messages", handle));
        Ok(())
    }

    /// Arrête les fils puis détruit mpv. Sans effet visible si mpv s'est déjà arrêté de lui-même.
    fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        // `quit` termine le cœur même si un client traîne ; l'erreur est normale si mpv a déjà disparu.
        let _ = self.mpv.command("quit", &[]);
        let deadline = Instant::now() + GENERATION_STOP_TIMEOUT;
        for (name, handle) in self.threads {
            if !join_with_timeout(handle, deadline.saturating_duration_since(Instant::now())) {
                report_error(&format!("lecteur: fil {name}"), &"toujours actif à l'arrêt");
            }
        }
    }
}

/// Surveille mpv : après un arrêt inattendu, le relance (budget `RestartPolicy`) et rejoue réglages, observations et média
/// à la dernière position connue. Budget épuisé : l'interface reçoit une fin de lecture en erreur.
fn supervise_player(ctx: PlayerContext, first: Generation, died: Receiver<String>) {
    let mut policy = RestartPolicy::new(MAX_RESTARTS, RESTART_WINDOW, RESTART_BASE_DELAY);
    let mut current = Some(first);
    while !SERVICES.is_shutdown() {
        let cause = match died.recv_timeout(THREAD_POLL) {
            Ok(cause) => cause,
            Err(flume::RecvTimeoutError::Timeout) => continue,
            Err(flume::RecvTimeoutError::Disconnected) => break,
        };
        report_error("lecteur: arrêt inattendu de mpv", &cause);
        if let Some(generation) = current.take() {
            generation.stop();
        }
        while died.try_recv().is_ok() {}
        current = restart_player(&ctx, &mut policy);
        if current.is_none() {
            report_error("lecteur", &"relances épuisées, mpv reste arrêté");
            notify_player_lost(&ctx);
            break;
        }
    }
    if let Some(generation) = current {
        generation.stop();
    }
}

/// Relance mpv selon le budget ; `None` quand il est épuisé (ou à la fermeture).
fn restart_player(ctx: &PlayerContext, policy: &mut RestartPolicy) -> Option<Generation> {
    loop {
        let delay = policy.on_failure(Instant::now())?;
        if !SERVICES.sleep(delay) {
            return None;
        }
        let preamble = lock(&ctx.resume, "reprise du lecteur").replay_messages();
        lock(&ctx.video_ready_state, "état du lecteur").reset_for_restart();
        match Generation::start(ctx, preamble) {
            Ok(generation) => return Some(generation),
            Err(error) => report_error("lecteur: relance de mpv échouée", &error),
        }
    }
}

/// L'interface apprend que la lecture est perdue pour de bon (fin en erreur).
fn notify_player_lost(ctx: &PlayerContext) {
    let response = PlayerResponse(
        "mpv-event-ended",
        PlayerEvent::End(PlayerEnded::from_end_reason(mpv_end_file_reason::Error)),
    );
    let _ = ctx
        .rpc_response_sender
        .send(RPCResponse::response_message(response.to_value()));
}

impl PartialUi for Player {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        // @TODO replace with `&mut self`?
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        let Some(window_handle) = parent
            .map(|parent| -> nwg::ControlHandle { parent.into() })
            .and_then(|parent| parent.hwnd())
        else {
            return Err(nwg::NwgError::initialization(
                "lecteur: fenêtre parente introuvable",
            ));
        };

        let (in_msg_sender, in_msg_receiver) = flume::unbounded();
        let (rpc_response_sender, rpc_response_receiver) = flume::unbounded();
        let (died_sender, died_receiver) = flume::unbounded();
        data.channel = ipc::Channel::new(Some((in_msg_sender, rpc_response_receiver)));

        let ctx = PlayerContext {
            window_handle: window_handle as isize,
            in_msg_receiver,
            rpc_response_sender,
            video_ready_state: Arc::new(Mutex::new(VideoReadyState::default())),
            gpu_video_processing: Arc::new(AtomicBool::new(false)),
            resume: Arc::new(Mutex::new(ResumeState::default())),
            died: died_sender,
        };
        let first = Generation::start(&ctx, Vec::new()).map_err(nwg::NwgError::initialization)?;
        SERVICES.spawn("mpv-supervisor", move || {
            supervise_player(ctx, first, died_receiver)
        });
        Ok(())
    }
}

fn create_mpv(window_handle: HWND) -> Result<Mpv, libmpv2::Error> {
    Mpv::with_initializer(|initializer| {
        macro_rules! set_property {
            ($name:literal, $value:expr) => {
                initializer
                    .set_property($name, $value)
                    .or_report(concat!("mpv: réglage impossible de ", $name))?;
            };
        }
        set_property!("wid", window_handle as i64);
        set_property!("title", "Laststream");
        set_property!("audio-client-name", "Laststream");
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
        options.extend(default_buffer_options());
        for (name, value) in options {
            if let Err(error) = initializer.set_property(name, value) {
                eprintln!("mpv: cannot set {name}={value}: {error:?}");
            }
        }
        Ok(())
    })
}

/// Tampon mpv par défaut (niveau « large »), appliqué à l'init avant tout `loadfile`.
/// `cache-secs` et `demuxer-readahead-secs` sont hauts : seule `demuxer-max-bytes` borne le tampon (une limite en secondes le plafonnerait pour les faibles débits).
/// L'UI peut le surcharger ensuite via `mpv-set-prop`.
fn default_buffer_options() -> [(&'static str, &'static str); 6] {
    [
        ("cache", "yes"),
        ("cache-secs", "3600"),
        ("demuxer-readahead-secs", "3600"),
        ("demuxer-max-bytes", "512MiB"),
        ("demuxer-max-back-bytes", "64MiB"),
        ("cache-pause-wait", "5"),
    ]
}

fn run_display_loop(
    mpv: Arc<Mpv>,
    window_handle: isize,
    gpu_video_processing: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    let mut last_state = None;

    while !stop.load(Ordering::SeqCst) {
        let state = current_display_output_state(&mpv, window_handle as HWND);
        let gpu = gpu_video_processing.load(Ordering::Relaxed);
        if last_state != Some((state, gpu)) {
            apply_display_output_mode(&mpv, state, gpu);
            last_state = Some((state, gpu));
        }
        // Dort 500 ms par tranches pour réagir vite à l'arrêt.
        for _ in 0..5 {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            thread::sleep(THREAD_POLL);
        }
    }
}

fn current_display_output_state(mpv: &Mpv, window_handle: HWND) -> DisplayOutputState {
    DisplayOutputState {
        mode: current_display_output_mode(window_handle),
        scale_percent: current_video_filter_scale(mpv, window_handle),
    }
}

fn current_display_output_mode(window_handle: HWND) -> DisplayOutputMode {
    // SAFETY: MonitorFromWindow accepte tout HWND (un handle invalide donne le moniteur le plus proche par défaut) et ne lit aucune mémoire de l'appelant.
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
    // SAFETY: MonitorFromWindow accepte tout HWND (un handle invalide donne le moniteur le plus proche par défaut) et ne lit aucune mémoire de l'appelant.
    let monitor = unsafe { MonitorFromWindow(window_handle, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return None;
    }

    // SAFETY: structure C de données brutes (entiers et pointeurs nuls admis) : tous les bits à zéro forment une valeur valide.
    let mut monitor_info: MONITORINFO = unsafe { mem::zeroed() };
    monitor_info.cbSize = mem::size_of::<MONITORINFO>() as DWORD;
    // SAFETY: `monitor` est non nul (testé plus haut) ; `monitor_info` est initialisée avec le `cbSize` correct et reste valide en écriture pendant l'appel.
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
            ("target-trc", "auto"),
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
    // SAFETY: structure C de données brutes (entiers et pointeurs nuls admis) : tous les bits à zéro forment une valeur valide.
    let mut monitor_info: MONITORINFOEXW = unsafe { mem::zeroed() };
    monitor_info.cbSize = mem::size_of::<MONITORINFOEXW>() as DWORD;

    let result =
        // SAFETY: `monitor` est non nul (testé par l'appelant) ; MONITORINFOEXW commence par un MONITORINFO, le cast est la convention de l'API, et `cbSize` annonce la taille réelle.
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
        // SAFETY: les deux compteurs sont des entiers locaux, valides en écriture pendant l'appel.
        let buffer_status = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        };
        if buffer_status != ERROR_SUCCESS as LONG {
            return None;
        }

        let mut paths =
            // SAFETY: structure C de données brutes (entiers et pointeurs nuls admis) : tous les bits à zéro forment une valeur valide.
            vec![unsafe { mem::zeroed::<DISPLAYCONFIG_PATH_INFO>() }; path_count as usize];
        let mut modes =
            // SAFETY: structure C de données brutes (entiers et pointeurs nuls admis) : tous les bits à zéro forment une valeur valide.
            vec![unsafe { mem::zeroed::<DISPLAYCONFIG_MODE_INFO>() }; mode_count as usize];
        // SAFETY: `paths` et `modes` contiennent au moins `path_count` et `mode_count` éléments initialisés ; les compteurs sont valides en écriture ; la topologie (pointeur nul) est facultative avec QDC_ONLY_ACTIVE_PATHS.
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
    // SAFETY: structure C de données brutes (entiers et pointeurs nuls admis) : tous les bits à zéro forment une valeur valide.
    let mut source_name: DISPLAYCONFIG_SOURCE_DEVICE_NAME = unsafe { mem::zeroed() };
    source_name.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
        _type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        size: mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
        adapterId: path.sourceInfo.adapterId,
        id: path.sourceInfo.id,
    };

    // SAFETY: l'en-tête est le premier champ de `source_name` ; son `type` et sa `size` décrivent la structure entière, valide en écriture pendant l'appel.
    let status = unsafe { DisplayConfigGetDeviceInfo(&mut source_name.header) };
    if status == ERROR_SUCCESS as LONG {
        Some(source_name)
    } else {
        None
    }
}

fn display_hdr_active(path: &DISPLAYCONFIG_PATH_INFO) -> Option<bool> {
    // SAFETY: structure C de données brutes (entiers et pointeurs nuls admis) : tous les bits à zéro forment une valeur valide.
    let mut color_info: DisplayconfigGetAdvancedColorInfo2 = unsafe { mem::zeroed() };
    color_info.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
        _type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2,
        size: mem::size_of::<DisplayconfigGetAdvancedColorInfo2>() as u32,
        adapterId: path.targetInfo.adapterId,
        id: path.targetInfo.id,
    };

    // SAFETY: l'en-tête est le premier champ de `color_info` (repr(C)) ; son `type` et sa `size` décrivent la structure entière, valide en écriture pendant l'appel.
    let status = unsafe { DisplayConfigGetDeviceInfo(&mut color_info.header) };
    if status == ERROR_SUCCESS as LONG {
        Some(color_info.active_color_mode == DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR)
    } else {
        None
    }
}

/// Réponse d'un changement de propriété ; une valeur que `communication.rs` ne sait pas convertir (nombre non fini,
/// JSON invalide) est journalisée et ignorée au lieu de tuer le fil des événements.
fn prop_change_response(name: &str, change: PropertyData) -> Option<PlayerResponse<'static>> {
    catch_unwind(AssertUnwindSafe(|| {
        PlayerResponse(
            "mpv-prop-change",
            PlayerEvent::PropChange(PlayerProprChange::from_name_value(name.to_string(), change)),
        )
    }))
    .map_err(|payload| {
        report_error(
            &format!("lecteur: valeur de {name} illisible"),
            &panic_message(payload.as_ref()),
        )
    })
    .ok()
}

fn run_event_loop(
    mpv_event_client: Mpv,
    observe_property_receiver: Receiver<ObserveProperty>,
    ctx: PlayerContext,
    stop: Arc<AtomicBool>,
) {
    let _ = mpv_event_client
        .disable_deprecated_events()
        .or_report("lecteur: événements obsolètes non désactivés");
    let _ = mpv_event_client
        .observe_property("time-pos", Format::Double, POSITION_OBSERVER_ID)
        .or_report("lecteur: position non observée");
    // La position n'est retenue qu'une fois le fichier courant chargé (pas celle du précédent).
    let mut position_valid = false;

    // -- Event handler loop --

    while !stop.load(Ordering::SeqCst) {
        for ObserveProperty { name, format } in observe_property_receiver.drain() {
            let _ = mpv_event_client
                .observe_property(&name, format, 0)
                .or_report(&format!("lecteur: propriété {name} non observée"));
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

        if let Event::PropertyChange {
            change: PropertyData::Double(seconds),
            reply_userdata: POSITION_OBSERVER_ID,
            ..
        } = &event
        {
            if position_valid {
                lock(&ctx.resume, "reprise du lecteur").record_position(*seconds);
            }
            continue;
        }

        if matches!(&event, Event::StartFile) {
            position_valid = false;
            lock(&ctx.video_ready_state, "état du lecteur").start_file();
            continue;
        }

        if matches!(&event, Event::FileLoaded) {
            position_valid = true;
            lock(&ctx.video_ready_state, "état du lecteur").file_loaded();
            continue;
        }

        if matches!(&event, Event::PlaybackRestart) {
            let load_id = lock(&ctx.video_ready_state, "état du lecteur").playback_restarted();
            if let Some(load_id) = load_id {
                send_video_ready(&ctx.rpc_response_sender, load_id, true);
            }
            continue;
        }

        // even if you don't do anything with the events, it is still necessary to empty the event loop
        let player_response = match event {
            Event::PropertyChange { name, change, .. } => {
                match prop_change_response(name, change) {
                    Some(response) => response,
                    None => continue,
                }
            }
            Event::EndFile(reason) => {
                // Fin de fichier ou erreur : plus rien à reprendre. (`Stop` accompagne un remplacement de média.)
                if reason == mpv_end_file_reason::Eof || reason == mpv_end_file_reason::Error {
                    lock(&ctx.resume, "reprise du lecteur").clear_media();
                }
                PlayerResponse(
                    "mpv-event-ended",
                    PlayerEvent::End(PlayerEnded::from_end_reason(reason)),
                )
            }
            Event::Shutdown => {
                signal_death(&ctx, &stop, "mpv s'est arrêté");
                break;
            }
            _ => continue,
        };

        if let Err(error) = ctx
            .rpc_response_sender
            .send(RPCResponse::response_message(player_response.to_value()))
        {
            eprintln!("failed to send RPCResponse: {error}");
            break;
        }
    }
}

fn run_message_loop(
    mpv: Arc<Mpv>,
    subtitle_mpv: Mpv,
    ctx: PlayerContext,
    stop: Arc<AtomicBool>,
    observe_property_sender: Sender<ObserveProperty>,
    preamble: Vec<InMsg>,
) {
    // -- Helpers --
    let (subtitle_request_sender, subtitle_request_receiver) = flume::unbounded();
    let subtitle_thread = create_subtitle_thread(subtitle_mpv, subtitle_request_receiver)
        .map_err(|error| report_error("lecteur: fil des sous-titres non démarré", &error))
        .ok();

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
        // Sans fil de sous-titres, la commande média part quand même : la lecture ne dépend pas de lui.
        if subtitle_request_sender
            .send(SubtitleWorkerRequest::BeginMediaChange(acknowledge))
            .is_err()
            || acknowledged.recv().is_err()
        {
            eprintln!("mpv: subtitle worker stopped before media change");
        }
        let parts: Vec<String> = cmd.into();
        if let Some((name, args)) = parts.split_first() {
            if let Some(loads_file) = media_transition {
                let load_id =
                    lock(&ctx.video_ready_state, "état du lecteur").begin_transition(loads_file);
                send_video_ready(&ctx.rpc_response_sender, load_id, false);
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

    let handle = |in_msg: InMsg| match in_msg {
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
            // JSON numbers arrive as f64, but libmpv refuses a double for an integer
            // property (aid, sid, vid... -> MPV_ERROR_PROPERTY_FORMAT): fall back to int64.
            let name = name.to_string();
            if mpv.set_property(&name, value).is_err() {
                match integral_value(value) {
                    Some(int) => set_property(&name, int, &mpv),
                    None => set_property(&name, value, &mpv),
                }
            }
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
                    current_display_output_state(&mpv, ctx.window_handle as HWND),
                    ctx.gpu_video_processing.load(Ordering::Relaxed),
                );
            }
        }
        InMsg(InMsgFn::MpvSetGpuVideoProcessing, InMsgArgs::Flag(enabled)) => {
            ctx.gpu_video_processing.store(enabled, Ordering::Relaxed);
            apply_display_output_mode(
                &mpv,
                current_display_output_state(&mpv, ctx.window_handle as HWND),
                enabled,
            );
        }
        InMsg(InMsgFn::MpvCommand, InMsgArgs::Cmd(cmd)) => {
            send_command(cmd);
        }
        msg => {
            eprintln!("MPV unsupported message: '{msg:?}'");
        }
    };

    // -- Reprise après une relance de mpv : ces messages passent avant ceux de l'interface --

    for in_msg in preamble {
        handle(in_msg);
    }

    // -- InMsg handler loop --

    while !stop.load(Ordering::SeqCst) {
        let msg = match ctx.in_msg_receiver.recv_timeout(THREAD_POLL) {
            Ok(msg) => msg,
            Err(flume::RecvTimeoutError::Timeout) => continue,
            Err(flume::RecvTimeoutError::Disconnected) => break,
        };
        let in_msg: InMsg = match serde_json::from_str(&msg) {
            Ok(in_msg) => in_msg,
            Err(error) => {
                eprintln!("cannot parse InMsg:{:?} {error:#}", msg);
                continue;
            }
        };
        lock(&ctx.resume, "reprise du lecteur").record_message(&in_msg);
        handle(in_msg);
    }

    // Les fils des sous-titres s'arrêtent quand leur canal se ferme.
    drop(subtitle_request_sender);
    if let Some(handle) = subtitle_thread {
        if !join_with_timeout(handle, GENERATION_STOP_TIMEOUT) {
            report_error("lecteur: fil des sous-titres", &"toujours actif à l'arrêt");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{default_buffer_options, ResumeState, VideoReadyState};
    use crate::stremio_app::stremio_player::communication::{BoolProp, FpProp};
    use crate::stremio_app::stremio_player::{CmdVal, InMsg, InMsgArgs, InMsgFn, PropKey, PropVal};
    use std::convert::TryFrom;

    fn set_prop(key: PropKey, value: PropVal) -> InMsg {
        InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(key, value))
    }

    fn load(url: &str) -> InMsg {
        InMsg(
            InMsgFn::MpvCommand,
            InMsgArgs::Cmd(CmdVal::Loadfile(url.to_owned())),
        )
    }

    #[test]
    fn restart_replays_settings_observations_then_resumes_at_the_known_position() {
        let mut resume = ResumeState::default();
        resume.record_message(&set_prop(PropKey::Fp(FpProp::Volume), PropVal::Num(40.0)));
        resume.record_message(&set_prop(PropKey::Fp(FpProp::Volume), PropVal::Num(70.0)));
        resume.record_message(&InMsg(
            InMsgFn::MpvObserveProp,
            InMsgArgs::ObProp(PropKey::Fp(FpProp::TimePos)),
        ));
        resume.record_message(&load("http://127.0.0.1/film.mkv"));
        resume.record_position(123.4567);

        assert_eq!(
            resume.replay_messages(),
            vec![
                set_prop(PropKey::Fp(FpProp::Volume), PropVal::Num(70.0)),
                InMsg(
                    InMsgFn::MpvObserveProp,
                    InMsgArgs::ObProp(PropKey::Fp(FpProp::TimePos))
                ),
                InMsg(
                    InMsgFn::MpvCommand,
                    InMsgArgs::Cmd(CmdVal::LoadfileAt(
                        "http://127.0.0.1/film.mkv".to_owned(),
                        "start=+123.457".to_owned()
                    ))
                ),
            ]
        );
    }

    #[test]
    fn resume_command_is_accepted_by_the_command_whitelist() {
        let mut resume = ResumeState::default();
        resume.record_message(&load("http://127.0.0.1/film.mkv"));
        resume.record_position(42.0);
        let Some(command) = resume.resume_command() else {
            panic!("une reprise est attendue");
        };
        let wire: Vec<String> = command.clone().into();
        assert_eq!(CmdVal::try_from(wire), Ok(command));
    }

    #[test]
    fn near_the_start_the_original_load_is_replayed_unchanged() {
        let mut resume = ResumeState::default();
        resume.record_message(&load("a.mkv"));
        assert_eq!(
            resume.resume_command(),
            Some(CmdVal::Loadfile("a.mkv".to_owned()))
        );
        resume.record_position(0.4);
        assert_eq!(
            resume.resume_command(),
            Some(CmdVal::Loadfile("a.mkv".to_owned()))
        );
    }

    #[test]
    fn a_new_load_forgets_the_old_position_and_stop_or_end_forget_the_media() {
        let mut resume = ResumeState::default();
        resume.record_message(&load("a.mkv"));
        resume.record_position(300.0);
        resume.record_message(&load("b.mkv"));
        assert_eq!(resume.position, None);

        resume.record_position(f64::NAN);
        assert_eq!(resume.position, None);

        resume.record_position(10.0);
        resume.record_message(&InMsg(InMsgFn::MpvCommand, InMsgArgs::Cmd(CmdVal::Stop)));
        assert_eq!(resume.resume_command(), None);

        resume.record_message(&load("c.mkv"));
        resume.clear_media();
        assert!(resume.replay_messages().is_empty());
        // Sans média, une position tardive est ignorée.
        resume.record_position(5.0);
        assert_eq!(resume.position, None);
    }

    #[test]
    fn observed_properties_are_not_duplicated() {
        let mut resume = ResumeState::default();
        let observe = InMsg(
            InMsgFn::MpvObserveProp,
            InMsgArgs::ObProp(PropKey::Bool(BoolProp::Pause)),
        );
        resume.record_message(&observe);
        resume.record_message(&observe);
        assert_eq!(resume.replay_messages(), vec![observe]);
    }

    #[test]
    fn restart_drops_pending_loads_but_keeps_load_numbers_increasing() {
        let mut state = VideoReadyState::default();
        let before = state.begin_transition(true);
        state.reset_for_restart();
        state.file_loaded();
        assert_eq!(state.playback_restarted(), None);
        let after = state.begin_transition(true);
        assert!(after > before);
        state.start_file();
        state.file_loaded();
        assert_eq!(state.playback_restarted(), Some(after));
    }

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

    #[test]
    fn default_buffer_table_is_the_large_level() {
        let table = default_buffer_options();
        assert_eq!(
            table,
            [
                ("cache", "yes"),
                ("cache-secs", "3600"),
                ("demuxer-readahead-secs", "3600"),
                ("demuxer-max-bytes", "512MiB"),
                ("demuxer-max-back-bytes", "64MiB"),
                ("cache-pause-wait", "5"),
            ]
        );
        let mut names: Vec<_> = table.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), table.len());
    }

    #[test]
    fn default_demuxer_max_bytes_is_a_mib_size() {
        let table = default_buffer_options();
        let value = table
            .iter()
            .find(|(name, _)| *name == "demuxer-max-bytes")
            .map(|(_, value)| *value)
            .expect("demuxer-max-bytes is set");
        let mib = value
            .strip_suffix("MiB")
            .and_then(|n| n.parse::<u64>().ok());
        assert_eq!(mib, Some(512));
    }
}
