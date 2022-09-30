use native_windows_gui as nwg;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::thread;
use std::time::Duration;
use win32job::Job;

const CREATE_NO_WINDOW: u32 = 0x08000000;

pub struct StremioServer {}

impl StremioServer {
    pub fn new() -> StremioServer {
        thread::spawn(move || {
            let job = Job::create().expect("Cannont create job");
            let mut info = job.query_extended_limit_info().expect("Cannont get info");
            info.limit_kill_on_job_close();
            job.set_extended_limit_info(&mut info).ok();
            job.assign_current_process().ok();
            loop {
                let child = Command::new("./stremio-runtime")
                    .arg("server.js")
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn();
                match child {
                    Ok(mut child) => {
                        // TODO: store somehow last few lines of the child's stdout/stderr instead of just waiting
                        child.wait().expect("Cannot wait for the server");
                    }
                    Err(err) => {
                        nwg::error_message(
                            "Stremio server",
                            format!("Cannot execute stremio-runtime: {}", &err).as_str(),
                        );
                        break;
                    }
                };
                // TODO: show error message with the child's stdout/stderr
                thread::sleep(Duration::from_millis(500));
                dbg!("Trying to restart the server...");
            }
        });
        StremioServer {}
    }
}
