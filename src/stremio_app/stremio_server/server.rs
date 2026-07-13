use crate::stremio_app::constants::{SRV_BUFFER_SIZE, SRV_LOG_SIZE, STREMIO_SERVER_DEV_MODE};
use native_windows_gui::{self as nwg, PartialUi};
use std::io::Write;
use std::{
    env, fs,
    io::Read,
    ops::Deref,
    os::windows::process::CommandExt,
    path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};
use winapi::um::{
    handleapi::CloseHandle,
    processthreadsapi::GetCurrentProcess,
    winbase::{CreateJobObjectA, CREATE_NO_WINDOW},
    winnt::{
        JobObjectExtendedLimitInformation, JOBOBJECT_BASIC_LIMIT_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};

#[derive(Default)]
pub struct StremioServer {
    development: bool,
    parent: nwg::ControlHandle,
    crash_notice: nwg::Notice,
    logs: Arc<Mutex<String>>,
    server_url: Arc<Mutex<Option<String>>>,
}

impl StremioServer {
    pub fn server_url(&self) -> Option<String> {
        self.server_url.lock().ok().and_then(|url| url.clone())
    }

    pub fn start(&self) -> Option<String> {
        if self.development {
            return None;
        }
        let (tx, rx) = flume::unbounded();
        let logs = self.logs.clone();
        let sender = self.crash_notice.sender();

        thread::spawn(move || {
            // Use Win32JobObject to kill the child process when the parent process is killed
            // With the JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK and JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE flags
            unsafe {
                let job_main_process = CreateJobObjectA(std::ptr::null_mut(), std::ptr::null_mut());
                if job_main_process.is_null() {
                    eprintln!(
                        "CreateJobObjectA failed: {}",
                        std::io::Error::last_os_error()
                    );
                } else {
                    let jeli = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
                        BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                            LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                                | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
                                | JOB_OBJECT_LIMIT_BREAKAWAY_OK,
                            ..std::mem::zeroed()
                        },
                        ..std::mem::zeroed()
                    };
                    if winapi::um::jobapi2::SetInformationJobObject(
                        job_main_process,
                        JobObjectExtendedLimitInformation,
                        &jeli as *const _ as *mut _,
                        std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    ) == 0
                    {
                        eprintln!(
                            "SetInformationJobObject failed: {}",
                            std::io::Error::last_os_error()
                        );
                    }
                    if winapi::um::jobapi2::AssignProcessToJobObject(
                        job_main_process,
                        GetCurrentProcess(),
                    ) == 0
                    {
                        eprintln!(
                            "AssignProcessToJobObject failed (parent likely already in a \
                             non-breakaway job): {}",
                            std::io::Error::last_os_error()
                        );
                        // Assignment failed, so this handle isn't backing a live
                        // kill-on-close relationship with our process — close it
                        // immediately instead of leaking it.
                        CloseHandle(job_main_process);
                    }
                    // On success the handle intentionally stays open for the lifetime of
                    // this process: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE fires when the last
                    // handle to the job closes, and closing it here (right after our own
                    // process was assigned to it) triggers that immediately. `start()` is
                    // only called once at launch and again on the rare crash-restart path,
                    // so this intentional per-call handle is bounded, not an unbounded leak.
                }
            }
            let mut path = env::current_exe()
                .and_then(fs::canonicalize)
                .expect("Cannot get the current executable path");
            path.pop();
            let lines = Arc::new(Mutex::new(String::new()));
            let runtime_path = path.clone().join(path::Path::new("stremio-runtime"));
            let server_path = path.clone().join(path::Path::new("server.js"));
            let child = Command::new(runtime_path)
                .arg(server_path)
                .creation_flags(CREATE_NO_WINDOW)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            match child {
                Ok(mut child) => {
                    let mut stdout = child.stdout.take().unwrap();
                    let out_lines = lines.clone();
                    let tx = tx.clone();
                    let out_thread = thread::spawn(move || {
                        let mut endpoint_sent = false;
                        // Accumulates output across reads so the readiness line is still
                        // found if "EngineFS server started at ..." straddles a read
                        // boundary; without this, a chunk split mid-line means neither
                        // read contains the full line and readiness is silently missed.
                        let mut pending = String::new();
                        loop {
                            let mut buffer = [0; SRV_BUFFER_SIZE];
                            let on = match stdout.read(&mut buffer[..]) {
                                Ok(0) => break,
                                Ok(n) => n,
                                Err(err) => {
                                    eprintln!("server stdout read error: {err}");
                                    break;
                                }
                            };
                            std::io::stdout().write_all(&buffer).ok();
                            let string_data = String::from_utf8_lossy(&buffer[..on]);
                            {
                                let lines = &mut *out_lines.lock().unwrap();
                                *lines += string_data.deref();
                                if !endpoint_sent {
                                    pending += string_data.deref();
                                    if let Some(line_end) = pending.rfind('\n') {
                                        let (complete, rest) = pending.split_at(line_end + 1);
                                        if let Some(http_endpoint) = complete
                                            .lines()
                                            .find(|line| {
                                                line.starts_with("EngineFS server started at")
                                            })
                                            .and_then(|line| line.split_whitespace().last())
                                        {
                                            println!("HTTP endpoint: {http_endpoint}");
                                            tx.send(http_endpoint.to_string()).ok();
                                            endpoint_sent = true;
                                        }
                                        pending = rest.to_string();
                                    }
                                }
                                *lines = lines
                                    .lines()
                                    .rev()
                                    .take(SRV_LOG_SIZE)
                                    .collect::<Vec<&str>>()
                                    .into_iter()
                                    .rev()
                                    .collect::<Vec<&str>>()
                                    .join("\n");
                            };
                        }
                    });

                    let mut stderr = child.stderr.take().unwrap();
                    let err_lines = lines.clone();
                    let err_thread = thread::spawn(move || {
                        let mut buffer = [0; SRV_BUFFER_SIZE];
                        loop {
                            let en = match stderr.read(&mut buffer[..]) {
                                Ok(0) => break,
                                Ok(n) => n,
                                Err(err) => {
                                    eprintln!("server stderr read error: {err}");
                                    break;
                                }
                            };
                            std::io::stderr().write_all(&buffer).ok();
                            let string_data = String::from_utf8_lossy(&buffer[..en]);
                            // eprint!("{:?}", &buffer);
                            {
                                let lines = &mut *err_lines.lock().unwrap();
                                *lines += string_data.deref();
                                *lines = lines
                                    .lines()
                                    .rev()
                                    .take(SRV_LOG_SIZE)
                                    .collect::<Vec<&str>>()
                                    .into_iter()
                                    .rev()
                                    .collect::<Vec<&str>>()
                                    .join("\n");
                            };
                        }
                    });
                    out_thread.join().ok();
                    err_thread.join().ok();
                }
                Err(err) => {
                    nwg::error_message(
                        "Stremio server",
                        format!("Cannot execute stremio-runtime: {}", &err).as_str(),
                    );
                }
            };

            {
                let mut logs = logs.lock().unwrap();
                *logs = lines.lock().unwrap().deref().to_string();
            }
            println!("Server terminated.");
            sender.notice();
        });

        // Wait for the server to start and keep the exact endpoint printed by server.js.
        let server_url = rx.recv().unwrap();
        if let Ok(mut stored_url) = self.server_url.lock() {
            *stored_url = Some(server_url.clone());
        }
        Some(server_url)
    }
}

impl PartialUi for StremioServer {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        if std::env::var(STREMIO_SERVER_DEV_MODE).unwrap_or("false".to_string()) == "true" {
            data.development = true;
        }

        data.parent = parent.expect("No parent window").into();

        nwg::Notice::builder()
            .parent(data.parent)
            .build(&mut data.crash_notice)
            .ok();
        let _ = data.start();
        println!("Stremio server started");
        Ok(())
    }
    fn process_event<'a>(
        &self,
        evt: nwg::Event,
        _evt_data: &nwg::EventData,
        handle: nwg::ControlHandle,
    ) {
        use nwg::Event as E;
        if evt == E::OnNotice && handle == self.crash_notice.handle {
            nwg::modal_error_message(
                self.parent,
                "Stremio server crash log",
                self.logs.lock().unwrap().deref(),
            );
            let _ = self.start();
        }
    }
}
