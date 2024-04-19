use native_windows_gui as nwg;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::thread;
use std::time::Duration;
use winapi::um::{
    processthreadsapi::GetCurrentProcess,
    winbase::CreateJobObjectA,
    winnt::{
        JobObjectExtendedLimitInformation, JOBOBJECT_BASIC_LIMIT_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};

const CREATE_NO_WINDOW: u32 = 0x08000000;

pub struct StremioServer {}

impl StremioServer {
    pub fn new() -> StremioServer {
        thread::spawn(move || {
            // Use Win32JobObject to kill the child process when the parent process is killed
            // With the JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK and JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE flags
            unsafe {
                let job_main_process = CreateJobObjectA(std::ptr::null_mut(), std::ptr::null_mut());
                let jeli = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
                    BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                        LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
                            | JOB_OBJECT_LIMIT_BREAKAWAY_OK,
                        ..std::mem::zeroed()
                    },
                    ..std::mem::zeroed()
                };
                winapi::um::jobapi2::SetInformationJobObject(
                    job_main_process,
                    JobObjectExtendedLimitInformation,
                    &jeli as *const _ as *mut _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                winapi::um::jobapi2::AssignProcessToJobObject(
                    job_main_process,
                    GetCurrentProcess(),
                );
            }
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
