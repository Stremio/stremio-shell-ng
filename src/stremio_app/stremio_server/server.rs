use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use winapi::um::handleapi::CloseHandle;
use winapi::um::processthreadsapi::{OpenProcess, TerminateProcess};
use winapi::um::winnt::PROCESS_TERMINATE;

pub struct StremioServer {
    pid_mutex: Arc<std::sync::Mutex<u32>>,
}

impl StremioServer {
    pub fn new() -> StremioServer {
        let server_pid_mutex = Arc::new(Mutex::new(0));
        let server_pid_mutex2 = server_pid_mutex.clone();
        thread::spawn(move || loop {
            let mut child = Command::new("node")
                .arg("server.js")
                .spawn()
                .expect("Cannot run the server");
            {
                let mut server_pid = server_pid_mutex2
                    .lock()
                    .expect("Trying to lock the mutex twice");
                *server_pid = child.id();
            };
            child.wait().expect("Cannot wait for the server");
            {
                let server_pid = server_pid_mutex2
                    .lock()
                    .expect("Trying to lock the mutex twice");
                if *server_pid == 0 {
                    dbg!("Exit server guard loop...");
                    break;
                }
            };
            thread::sleep(Duration::from_millis(500));
            dbg!("Trying to restart the server...");
        });
        StremioServer {
            pid_mutex: server_pid_mutex,
        }
    }
    pub fn try_kill(&self) {
        dbg!("Trying to kill the server...");
        let servr_pid_mutex = self.pid_mutex.clone();
        let mut server_pid = servr_pid_mutex
            .lock()
            .expect("Trying to lock the mutex twice");
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, *server_pid);
            if !handle.is_null() {
                TerminateProcess(handle, 101);
                CloseHandle(handle);
            }
        }
        *server_pid = 0;
    }
}
