use winapi::shared::minwindef::{BOOL, LPARAM, TRUE};
use winapi::shared::windef::HWND;
use winapi::um::winuser::{EnumChildWindows, GetClassNameA};

struct EnumState {
    found: Option<HWND>,
}

fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0i8; 256];
    let length = unsafe { GetClassNameA(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    if length <= 0 {
        return String::new();
    }
    let bytes = buffer[..length as usize]
        .iter()
        .map(|value| *value as u8)
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = &mut *(lparam as *mut EnumState);
    if class_name(hwnd).eq_ignore_ascii_case("mpv") {
        state.found = Some(hwnd);
        return 0;
    }
    TRUE
}

/// Find MPV's own child window without guessing at unrelated WebView children.
pub fn find_mpv_child_hwnd(parent: HWND) -> Option<HWND> {
    let mut state = EnumState { found: None };
    unsafe {
        EnumChildWindows(
            parent,
            Some(enum_proc),
            &mut state as *mut EnumState as LPARAM,
        );
    }
    state.found
}
