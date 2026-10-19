//! Local Laststream service (`lastaddon.cjs`), run beside the Stremio streaming server.
//!
//! The service is started with the shell, must print `LASTADDON_READY port=<n>` on stdout
//! within `READY_TIMEOUT`, is restarted once if it crashes afterwards, and is killed with the
//! shell through a job object.

use crate::stremio_app::lastaddon_import::{import_developer_data, import_source};
use crate::stremio_app::{
    constants::{SRV_BUFFER_SIZE, SRV_LOG_SIZE, STREMIO_SERVER_DEV_MODE},
    stremio_server::job::{self, ChildJob},
};
use flume::{Receiver, Sender, TryRecvError};
use native_windows_gui::{self as nwg, PartialUi};
use std::{
    collections::VecDeque,
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use winapi::um::winbase::CREATE_NO_WINDOW;

const READY_PREFIX: &str = "LASTADDON_READY";
const READY_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const FIRST_PORT: u16 = 7000;
const PORT_CANDIDATES: u16 = 100;
const MAX_RESTARTS: u32 = 1;
/// Permanent service (scheduled task) the shell reuses instead of launching its own lastAddon.
const SERVICE_URL: &str = "http://127.0.0.1:7000";
const PROBE_ATTEMPTS: u32 = 3;
const PROBE_TIMEOUT: Duration = Duration::from_millis(800);
const PROBE_RETRY_DELAY: Duration = Duration::from_millis(900);

type Logs = Arc<Mutex<VecDeque<String>>>;

#[derive(Debug, PartialEq, Eq)]
pub enum AddonEvent {
    /// The service listens on this URL.
    Ready(String),
    /// The service cannot run; the message is shown to the user.
    Failed(String),
}

enum Control {
    Start,
    Stop,
}

/// Outcome of one launch of the service.
enum Attempt {
    Stopped,
    /// Never became ready, or failed to start.
    Failed(String),
    /// Was ready, then stopped on its own.
    Crashed(String),
}

/// Why a launch ended while it was supervised.
enum Exit {
    Requested,
    Lost(String),
    TimedOut,
}

#[derive(Default)]
pub struct LastAddon {
    pub notice: nwg::Notice,
    process: Option<AddonProcess>,
}

impl LastAddon {
    pub fn start(&self) {
        if let Some(process) = &self.process {
            process.start();
        }
    }

    pub fn events(&self) -> impl Iterator<Item = AddonEvent> + '_ {
        self.process.iter().flat_map(AddonProcess::events)
    }
}

impl PartialUi for LastAddon {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        let parent = parent.expect("No parent window").into();
        nwg::Notice::builder()
            .parent(parent)
            .build(&mut data.notice)?;

        if env::var(STREMIO_SERVER_DEV_MODE).as_deref() != Ok("true") {
            let sender = data.notice.sender();
            data.process = Some(AddonProcess::with_probe(
                service_command,
                READY_TIMEOUT,
                || {
                    existing_service(PROBE_ATTEMPTS, PROBE_RETRY_DELAY, || {
                        fetch_health(SERVICE_URL, PROBE_TIMEOUT)
                    })
                },
                move || {
                    sender.notice();
                },
            ));
        }
        Ok(())
    }
}

pub struct AddonProcess {
    control: Sender<Control>,
    events: Receiver<AddonEvent>,
}

impl AddonProcess {
    #[cfg(test)]
    fn new(
        factory: impl FnMut() -> Result<Command, String> + Send + 'static,
        timeout: Duration,
        notify: impl Fn() + Send + 'static,
    ) -> Self {
        Self::with_probe(factory, timeout, || false, notify)
    }

    /// `probe` tells whether a permanent lastAddon already answers; then none is launched and
    /// the shell only reuses it (it is never stopped by the shell).
    fn with_probe(
        mut factory: impl FnMut() -> Result<Command, String> + Send + 'static,
        timeout: Duration,
        probe: impl Fn() -> bool + Send + 'static,
        notify: impl Fn() + Send + 'static,
    ) -> Self {
        let (control, control_rx) = flume::unbounded();
        let (event_tx, events) = flume::unbounded();
        thread::spawn(move || {
            let emit = |event: AddonEvent| {
                if event_tx.send(event).is_ok() {
                    notify();
                }
            };
            // Nothing runs before the shell asks for it; Retry after a failure sends Start again.
            while let Ok(Control::Start) = control_rx.recv() {
                if probe() {
                    emit(AddonEvent::Ready(format!("{SERVICE_URL}/")));
                    // Nothing to supervise or kill: wait for the shell to close.
                    while let Ok(Control::Start) = control_rx.recv() {}
                    return;
                }
                let mut restarts = 0;
                loop {
                    match run_attempt(&mut factory, timeout, &control_rx, &emit) {
                        Attempt::Stopped => return,
                        Attempt::Crashed(error) if restarts < MAX_RESTARTS => {
                            restarts += 1;
                            eprintln!("{error} Redémarrage du service Laststream.");
                        }
                        Attempt::Crashed(error) | Attempt::Failed(error) => {
                            emit(AddonEvent::Failed(error));
                            break;
                        }
                    }
                }
            }
        });
        Self { control, events }
    }

    fn start(&self) {
        self.control.send(Control::Start).ok();
    }

    fn events(&self) -> impl Iterator<Item = AddonEvent> + '_ {
        self.events.try_iter()
    }
}

impl Drop for AddonProcess {
    fn drop(&mut self) {
        self.control.send(Control::Stop).ok();
    }
}

fn run_attempt(
    factory: &mut impl FnMut() -> Result<Command, String>,
    timeout: Duration,
    control: &Receiver<Control>,
    emit: &impl Fn(AddonEvent),
) -> Attempt {
    let mut command = match factory() {
        Ok(command) => command,
        Err(error) => return Attempt::Failed(error),
    };
    let mut child = match command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return Attempt::Failed(format!("Impossible de lancer stremio-runtime : {error}"))
        }
    };
    let job = match ChildJob::assign(&child) {
        Ok(job) => job,
        Err(error) => {
            child.kill().ok();
            child.wait().ok();
            return Attempt::Failed(format!(
                "Impossible de superviser le service Laststream : {error}"
            ));
        }
    };

    let logs: Logs = Arc::new(Mutex::new(VecDeque::new()));
    let (ready_tx, ready_rx) = flume::unbounded();
    let stdout = child.stdout.take().expect("Piped Laststream stdout");
    let stderr = child.stderr.take().expect("Piped Laststream stderr");
    let stdout_thread = spawn_reader(stdout, true, ready_tx.clone(), logs.clone());
    let stderr_thread = spawn_reader(stderr, false, ready_tx, logs.clone());

    let started = Instant::now();
    let mut ready_port = None;
    let exit = loop {
        match control.try_recv() {
            Ok(Control::Stop) | Err(TryRecvError::Disconnected) => break Exit::Requested,
            Ok(Control::Start) | Err(TryRecvError::Empty) => {}
        }
        take_ready(&ready_rx, &mut ready_port, emit);
        match child.try_wait() {
            Ok(Some(status)) => {
                break Exit::Lost(format!("Le service Laststream s'est arrêté ({status})."))
            }
            Ok(None) => {}
            Err(error) => {
                break Exit::Lost(format!(
                    "Impossible de lire l'état du service Laststream : {error}"
                ))
            }
        }
        if ready_port.is_none() && started.elapsed() >= timeout {
            break Exit::TimedOut;
        }
        thread::sleep(POLL_INTERVAL);
    };

    // Dropping the job also stops descendants that still hold the output pipes.
    drop(job);
    child.kill().ok();
    child.wait().ok();
    stdout_thread.join().ok();
    stderr_thread.join().ok();
    // READY may have been printed just before the exit; the reader has finished by now.
    take_ready(&ready_rx, &mut ready_port, emit);

    match exit {
        Exit::Requested => Attempt::Stopped,
        Exit::Lost(reason) if ready_port.is_some() => Attempt::Crashed(with_logs(reason, &logs)),
        Exit::Lost(reason) => Attempt::Failed(with_logs(reason, &logs)),
        Exit::TimedOut => Attempt::Failed(with_logs(
            format!(
                "Le service Laststream n'a pas signalé {READY_PREFIX} dans les {} secondes.",
                timeout.as_secs()
            ),
            &logs,
        )),
    }
}

fn spawn_reader(
    output: impl Read + Send + 'static,
    stdout: bool,
    ready: Sender<u16>,
    logs: Logs,
) -> JoinHandle<()> {
    thread::spawn(move || read_output(output, stdout, &ready, &logs))
}

fn read_output(output: impl Read, stdout: bool, ready: &Sender<u16>, logs: &Logs) {
    for line in BufReader::with_capacity(SRV_BUFFER_SIZE, output).lines() {
        let Ok(line) = line else { return };
        if stdout {
            writeln!(std::io::stdout(), "{line}").ok();
        } else {
            writeln!(std::io::stderr(), "{line}").ok();
        }
        {
            let mut logs = logs.lock().unwrap();
            logs.push_back(line.clone());
            if logs.len() > SRV_LOG_SIZE {
                logs.pop_front();
            }
        }
        if let (true, Some(port)) = (stdout, parse_ready(&line)) {
            ready.send(port).ok();
        }
    }
}

fn take_ready(ready_rx: &Receiver<u16>, ready_port: &mut Option<u16>, emit: &impl Fn(AddonEvent)) {
    if ready_port.is_none() {
        if let Ok(port) = ready_rx.try_recv() {
            *ready_port = Some(port);
            emit(AddonEvent::Ready(format!("http://127.0.0.1:{port}/")));
        }
    }
}

fn with_logs(error: String, logs: &Logs) -> String {
    let logs = logs.lock().unwrap();
    if logs.is_empty() {
        error
    } else {
        let output = logs.iter().cloned().collect::<Vec<_>>().join("\n");
        format!("{error}\n\nDernières sorties du service :\n{output}")
    }
}

fn parse_ready(line: &str) -> Option<u16> {
    let fields = line.trim().strip_prefix(READY_PREFIX)?;
    fields
        .split_whitespace()
        .find_map(|field| field.strip_prefix("port=")?.parse::<u16>().ok())
        .filter(|port| *port != 0)
}

/// True when `body` is the `/health` answer of lastAddon: a JSON object with `"ok": true`.
fn is_addon_health(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("ok").and_then(serde_json::Value::as_bool))
        == Some(true)
}

/// Body of `GET {base}/health` when it answers 2xx within `timeout`.
fn fetch_health(base: &str, timeout: Duration) -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .no_proxy()
        .build()
        .ok()?;
    let response = client.get(format!("{base}/health")).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.text().ok()
}

/// Polls `fetch` up to `attempts` times (waiting `delay` between them, so a service that starts
/// at logon has time to come up) and tells whether a lastAddon answered.
fn existing_service(
    attempts: u32,
    delay: Duration,
    mut fetch: impl FnMut() -> Option<String>,
) -> bool {
    for attempt in 0..attempts {
        if attempt > 0 {
            thread::sleep(delay);
        }
        if fetch().is_some_and(|body| is_addon_health(&body)) {
            return true;
        }
    }
    false
}

fn free_port_from(first: u16) -> Option<u16> {
    (first..first.saturating_add(PORT_CANDIDATES)).find(|port| is_free(*port))
}

/// Another process may hold a port that a bind alone does not reveal on Windows, so a port
/// that still accepts connections is treated as taken.
fn is_free(port: u16) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_err()
        && TcpListener::bind(address).is_ok()
}

/// The production command: the bundled Node 22 runs `lastaddon/lastaddon.cjs` next to the shell.
/// Node 22 is required for `node:sqlite`, which the Node runtime of the streaming server lacks.
fn service_command() -> Result<Command, String> {
    job::protect_shell()
        .map_err(|error| format!("Impossible de superviser les processus enfants : {error}"))?;
    let port = free_port_from(FIRST_PORT)
        .ok_or_else(|| format!("Aucun port local libre à partir de {FIRST_PORT}."))?;
    let mut directory = env::current_exe()
        .map_err(|error| format!("Impossible de localiser l'installation : {error}"))?;
    directory.pop();
    let data_dir = dirs::config_dir()
        .ok_or_else(|| "Impossible de localiser le dossier AppData.".to_string())?
        .join("Laststream")
        .join("lastaddon");
    fs::create_dir_all(&data_dir)
        .map_err(|error| format!("Impossible de créer {} : {error}", data_dir.display()))?;

    // First run only: the developer data is copied before the service starts, never over existing data.
    if let Some(source) = import_source() {
        match import_developer_data(&source, &data_dir) {
            Ok(true) => println!("import effectué"),
            Ok(false) => {}
            Err(error) => eprintln!("import impossible ({:?})", error.kind()),
        }
    }

    // The secret of the service is kept in its data folder, so it survives updates.
    Ok(addon_command(&directory, &data_dir, port))
}

/// Path of the bundled ffmpeg (next to the shell). lastAddon uses it for the seek-bar thumbnails and
/// disables them on its own when the file is missing.
fn ffmpeg_path(directory: &Path) -> PathBuf {
    directory.join("ffmpeg.exe")
}

/// The command line and environment of the embedded lastAddon, for an installation `directory`.
fn addon_command(directory: &Path, data_dir: &Path, port: u16) -> Command {
    let mut command = Command::new(directory.join("lastaddon").join("node.exe"));
    command
        .arg(directory.join("lastaddon").join("lastaddon.cjs"))
        .env("PORT", port.to_string())
        .env("LASTADDON_UI_DIR", directory.join("ui"))
        .env("LASTADDON_DATA_DIR", data_dir)
        .env("LASTADDON_ENV_FILE", data_dir.join(".env"))
        .env("LASTADDON_FFMPEG", ffmpeg_path(directory))
        .env(
            "LASTADDON_ALLOWED_ORIGINS",
            format!("http://127.0.0.1:{port}"),
        )
        .creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(10);

    fn runtime() -> PathBuf {
        let directory = if cfg!(target_arch = "aarch64") {
            "bin-arm64"
        } else {
            "bin"
        };
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(directory)
            .join("stremio-runtime.exe")
    }

    fn script(source: &str) -> Command {
        let mut command = Command::new(runtime());
        command.args(["-e", source]);
        command
    }

    fn next_event(process: &AddonProcess) -> AddonEvent {
        process
            .events
            .recv_timeout(TEST_TIMEOUT)
            .expect("Expected an addon event")
    }

    #[test]
    fn passes_the_bundled_ffmpeg_next_to_the_shell() {
        let directory = Path::new(r"C:\Laststream");
        let command = addon_command(directory, Path::new(r"C:\Data"), 7000);
        let value = command
            .get_envs()
            .find(|(name, _)| *name == "LASTADDON_FFMPEG")
            .and_then(|(_, value)| value)
            .expect("LASTADDON_FFMPEG must be set");
        assert_eq!(value, Path::new(r"C:\Laststream\ffmpeg.exe").as_os_str());
        assert_eq!(ffmpeg_path(directory), directory.join("ffmpeg.exe"));
    }

    #[test]
    fn recognises_only_the_lastaddon_health_answer() {
        assert!(is_addon_health(r#"{"ok":true}"#));
        assert!(is_addon_health(r#"{ "ok": true, "x": 1 }"#));
        assert!(!is_addon_health(r#"{"ok":false}"#));
        assert!(!is_addon_health(r#"{"status":"up"}"#));
        assert!(!is_addon_health("OK"));
        assert!(!is_addon_health("<html>ok</html>"));
        assert!(!is_addon_health(""));
    }

    #[test]
    fn reuses_the_service_when_it_answers_even_late() {
        let mut calls = 0;
        let found = existing_service(3, Duration::ZERO, || {
            calls += 1;
            (calls == 2).then(|| r#"{"ok":true}"#.to_string())
        });
        assert!(found);
        assert_eq!(calls, 2);
    }

    #[test]
    fn launches_its_own_when_nothing_answers() {
        let mut calls = 0;
        let found = existing_service(3, Duration::ZERO, || {
            calls += 1;
            None
        });
        assert!(!found);
        assert_eq!(calls, 3);
    }

    #[test]
    fn launches_its_own_when_another_service_answers() {
        assert!(!existing_service(2, Duration::ZERO, || Some(
            "<html>other</html>".to_string()
        )));
        assert!(!existing_service(2, Duration::ZERO, || Some(
            r#"{"ok":false}"#.to_string()
        )));
    }

    #[test]
    fn fetches_health_from_a_real_listener_and_ignores_a_closed_port() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("Bind a test port");
        let port = listener.local_addr().expect("Address").port();
        let server = thread::spawn(move || {
            // `is_free` in other tests connects to nearby ports and closes at once: such a probe
            // carries no request, so it is skipped instead of using up the one answer served here.
            for stream in listener.incoming() {
                let mut stream = stream.expect("Accept");
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                while !request.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(read) if read > 0 => request.extend_from_slice(&buffer[..read]),
                        _ => break,
                    }
                }
                if request.is_empty() {
                    continue;
                }
                let body = r#"{"ok":true}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .ok();
                break;
            }
        });
        let base = format!("http://127.0.0.1:{port}");
        let body = fetch_health(&base, Duration::from_secs(3)).expect("Health answer");
        assert!(is_addon_health(&body));
        server.join().ok();
        assert_eq!(fetch_health(&base, Duration::from_millis(300)), None);
    }

    #[test]
    fn reports_ready_without_launching_when_the_service_exists() {
        let process = AddonProcess::with_probe(
            || panic!("The embedded lastAddon must not be launched"),
            TEST_TIMEOUT,
            || true,
            || {},
        );
        process.start();
        assert_eq!(
            next_event(&process),
            AddonEvent::Ready("http://127.0.0.1:7000/".to_string())
        );
    }

    #[test]
    fn parses_only_valid_ready_lines() {
        assert_eq!(parse_ready("LASTADDON_READY port=7000"), Some(7000));
        assert_eq!(
            parse_ready("  LASTADDON_READY port=7001 extra=1\r"),
            Some(7001)
        );
        assert_eq!(parse_ready("LASTADDON_READY port=0"), None);
        assert_eq!(parse_ready("LASTADDON_READY port=70000"), None);
        assert_eq!(parse_ready("LASTADDON_READY"), None);
        assert_eq!(parse_ready("Service ready on port=7000"), None);
    }

    #[test]
    fn never_picks_a_port_that_already_accepts_connections() {
        let occupied = TcpListener::bind(("127.0.0.1", 0)).expect("Bind a test port");
        let port = occupied.local_addr().expect("Test port address").port();
        assert_ne!(free_port_from(port), Some(port));
        assert!(free_port_from(port).is_some_and(|free| free > port));
    }

    #[test]
    fn reports_failure_when_the_command_cannot_start() {
        let process = AddonProcess::new(
            || {
                Ok(Command::new(
                    runtime().with_file_name("missing-runtime.exe"),
                ))
            },
            TEST_TIMEOUT,
            || {},
        );
        process.start();
        match next_event(&process) {
            AddonEvent::Failed(details) => {
                assert!(details.contains("Impossible de lancer"), "{}", details)
            }
            event => panic!("Expected failure, got {:?}", event),
        }
    }

    #[test]
    fn fails_without_ready_within_the_timeout() {
        let process = AddonProcess::new(
            || Ok(script("setInterval(() => {}, 1000);")),
            Duration::from_secs(1),
            || {},
        );
        process.start();
        match next_event(&process) {
            AddonEvent::Failed(details) => assert!(details.contains(READY_PREFIX), "{}", details),
            event => panic!("Expected failure, got {:?}", event),
        }
    }

    #[test]
    fn keeps_the_output_of_a_service_that_exits_before_ready() {
        // Node writes to pipes asynchronously, so the exit waits for the write to be flushed.
        let process = AddonProcess::new(
            || {
                Ok(script(
                    r#"process.stderr.write('Startup fixture failed\n', () => process.exit(7));"#,
                ))
            },
            TEST_TIMEOUT,
            || {},
        );
        process.start();
        match next_event(&process) {
            AddonEvent::Failed(details) => {
                assert!(details.contains("Startup fixture failed"), "{}", details);
                assert!(details.contains("arrêté"), "{}", details);
            }
            event => panic!("Expected failure, got {:?}", event),
        }
    }

    #[test]
    fn restarts_once_after_a_crash_then_reports_ready_again() {
        let mut first = true;
        let process = AddonProcess::new(
            move || {
                let source = if first {
                    "console.log('LASTADDON_READY port=47101'); setTimeout(() => process.exit(3), 300);"
                } else {
                    "console.log('LASTADDON_READY port=47102'); setInterval(() => {}, 1000);"
                };
                first = false;
                Ok(script(source))
            },
            TEST_TIMEOUT,
            || {},
        );
        process.start();
        assert_eq!(
            next_event(&process),
            AddonEvent::Ready("http://127.0.0.1:47101/".to_string())
        );
        assert_eq!(
            next_event(&process),
            AddonEvent::Ready("http://127.0.0.1:47102/".to_string())
        );
        assert!(process
            .events
            .recv_timeout(Duration::from_millis(300))
            .is_err());
    }

    #[test]
    fn gives_up_after_the_restart_budget_is_spent() {
        let process = AddonProcess::new(
            || {
                Ok(script(
                    "console.log('LASTADDON_READY port=47111'); setTimeout(() => process.exit(5), 200);",
                ))
            },
            TEST_TIMEOUT,
            || {},
        );
        process.start();
        assert!(matches!(next_event(&process), AddonEvent::Ready(_)));
        assert!(matches!(next_event(&process), AddonEvent::Ready(_)));
        match next_event(&process) {
            AddonEvent::Failed(details) => assert!(details.contains("arrêté"), "{}", details),
            event => panic!("Expected failure, got {:?}", event),
        }
    }

    #[test]
    fn stopping_kills_the_service_and_releases_its_port() {
        const PORT: u16 = 47121;
        let process = AddonProcess::new(
            || {
                Ok(script(
                    "require('net').createServer().listen(47121, '127.0.0.1', () => console.log('LASTADDON_READY port=47121'));",
                ))
            },
            TEST_TIMEOUT,
            || {},
        );
        process.start();
        assert_eq!(
            next_event(&process),
            AddonEvent::Ready(format!("http://127.0.0.1:{PORT}/"))
        );
        drop(process);
        let deadline = Instant::now() + TEST_TIMEOUT;
        while TcpListener::bind(("127.0.0.1", PORT)).is_err() {
            assert!(
                Instant::now() < deadline,
                "Service still owns port {}",
                PORT
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}
