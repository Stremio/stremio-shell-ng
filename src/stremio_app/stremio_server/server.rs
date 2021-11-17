use std::process::Command;
use std::thread;
use std::time::Duration;
use win32job::Job;
use std::os::windows::process::CommandExt;

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
                let mut child = Command::new("node")
                    .arg("server.js")
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                    .expect("Cannot run the server");
                child.wait().expect("Cannot wait for the server");
                thread::sleep(Duration::from_millis(500));
                dbg!("Trying to restart the server...");
            }
        });
        StremioServer {}
    }
}
