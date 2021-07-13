use std::process::Command;
use std::thread;

use std::sync::{Arc, Mutex};
use std::time::Duration;
use winapi::um::processthreadsapi::{OpenProcess, TerminateProcess};
use winapi::um::winnt::PROCESS_TERMINATE;
use winapi::um::handleapi::CloseHandle;

pub struct StremioServer {
    should_stop: Arc<std::sync::Mutex<u32>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl StremioServer {
    pub fn new() -> StremioServer {
        let server_pid_mutex = Arc::new(Mutex::new(0));
        let server_pid_mutex2 = server_pid_mutex.clone();
        StremioServer {
            should_stop: server_pid_mutex,
            handle: Some(thread::spawn(move || loop {
                let mut child = Command::new("node")
                    .arg("server.js")
                    .spawn()
                    .expect("Cannot run the server");
                {
                    let mut kill_request = server_pid_mutex2.lock().unwrap();
                    *kill_request = child.id();
                };
                let _status = child.wait().expect("Cannot wait for the server");
                let kill_request = server_pid_mutex2.lock().unwrap();
                if *kill_request == 0 {
                    dbg!("Exit server guard loop...");
                    break;
                }
                thread::sleep(Duration::from_millis(500));
                dbg!("Trying to restart the server...");
            })),
        }
    }
    pub fn try_kill(mut self) {
        dbg!("Trying to kill the server...");
        let should_stop = self.should_stop.clone();
        let mut tremination_request = should_stop.lock().unwrap();
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, *tremination_request);
            if !handle.is_null() {
                TerminateProcess(handle, 101);
                CloseHandle(handle);
            }
        }
        *tremination_request = 0;

        drop(self.handle.take())
        // .map(thread::JoinHandle::join);
    }
}

// impl Drop for StremioServer {
//     fn drop(&mut self) {
//         dbg!("Server dropped!");
//         self.try_kill();
//     }
// }
