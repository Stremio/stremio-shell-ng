use native_windows_gui as nwg;
use std::{
    env, fs, os::windows::process::CommandExt, path, process::Command, thread, time::Duration,
};
use winapi::um::{
    processthreadsapi::GetCurrentProcess,
    winbase::{CreateJobObjectA, CREATE_NO_WINDOW},
    winnt::{
        JobObjectExtendedLimitInformation, JOBOBJECT_BASIC_LIMIT_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};

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
            let mut path = env::current_exe()
                .and_then(fs::canonicalize)
                .expect("Cannot get the current executable path");
            path.pop();
            loop {
                let runtime_path = path.clone().join(path::Path::new("stremio-runtime"));
                let server_path = path.clone().join(path::Path::new("server.js"));
                let child = Command::new(runtime_path)
                    .arg(server_path)
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
