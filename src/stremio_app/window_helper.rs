use winapi::shared::windef::HWND__;
use winapi::um::winuser::{
    GetForegroundWindow, GetSystemMetrics, GetWindowLongA, GetWindowRect, IsIconic, IsZoomed,
    SetWindowLongA, SetWindowPos, GWL_EXSTYLE, GWL_STYLE, HWND_NOTOPMOST, HWND_TOPMOST,
    SM_CXSCREEN, SM_CYSCREEN, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, WS_CAPTION,
    WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME, WS_EX_STATICEDGE, WS_EX_TOPMOST, WS_EX_WINDOWEDGE,
    WS_THICKFRAME,
};

// https://doc.qt.io/qt-5/qt.html#WindowState-enum
bitflags! {
    struct WindowState: u8 {
        const MINIMIZED = 0x01;
        const MAXIMIZED = 0x02;
        const FULL_SCREEN = 0x04;
        const ACTIVE = 0x08;
    }
}

#[derive(Default, Clone)]
pub struct WindowStyle {
    pub full_screen: bool,
    pub pos: (i32, i32),
    pub size: (u32, u32),
    pub style: i32,
    pub ex_style: i32,
}

impl WindowStyle {
    pub fn get_window_state(self, hwnd: *mut HWND__) -> u32 {
        let mut state: WindowState = WindowState::empty();
        if 0 != unsafe { IsIconic(hwnd) } {
            state |= WindowState::MINIMIZED;
        }
        if 0 != unsafe { IsZoomed(hwnd) } {
            state |= WindowState::MAXIMIZED;
        }
        if hwnd == unsafe { GetForegroundWindow() } {
            state |= WindowState::ACTIVE
        }
        if self.full_screen {
            state |= WindowState::FULL_SCREEN;
        }
        state.bits() as u32
    }
    pub fn toggle_full_screen(&mut self, hwnd: *mut HWND__) {
        if self.full_screen {
            let topmost = if self.ex_style as u32 & WS_EX_TOPMOST == WS_EX_TOPMOST {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            };
            unsafe {
                SetWindowLongA(hwnd, GWL_STYLE, self.style);
                SetWindowLongA(hwnd, GWL_EXSTYLE, self.ex_style);
                SetWindowPos(
                    hwnd,
                    topmost,
                    self.pos.0,
                    self.pos.1,
                    self.size.0 as i32,
                    self.size.1 as i32,
                    SWP_FRAMECHANGED,
                );
            }
            self.full_screen = false;
        } else {
            unsafe {
                let mut rect = std::mem::zeroed();
                GetWindowRect(hwnd, &mut rect);
                self.pos = (rect.left, rect.top);
                self.size = (
                    (rect.right - rect.left) as u32,
                    (rect.bottom - rect.top) as u32,
                );
                self.style = GetWindowLongA(hwnd, GWL_STYLE);
                self.ex_style = GetWindowLongA(hwnd, GWL_EXSTYLE);
                SetWindowLongA(
                    hwnd,
                    GWL_STYLE,
                    self.style & !(WS_CAPTION as i32 | WS_THICKFRAME as i32),
                );
                SetWindowLongA(
                    hwnd,
                    GWL_EXSTYLE,
                    self.ex_style
                        & !(WS_EX_DLGMODALFRAME as i32
                            | WS_EX_WINDOWEDGE as i32
                            | WS_EX_CLIENTEDGE as i32
                            | WS_EX_STATICEDGE as i32),
                );
                SetWindowPos(
                    hwnd,
                    HWND_NOTOPMOST,
                    0,
                    0,
                    GetSystemMetrics(SM_CXSCREEN),
                    GetSystemMetrics(SM_CYSCREEN),
                    SWP_FRAMECHANGED,
                );
            }
            self.full_screen = true;
        }
    }
    pub fn toggle_topmost(&mut self, hwnd: *mut HWND__) {
        let topmost = if unsafe { GetWindowLongA(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST
            == WS_EX_TOPMOST
        {
            HWND_NOTOPMOST
        } else {
            HWND_TOPMOST
        };
        unsafe {
            SetWindowPos(
                hwnd,
                topmost,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_FRAMECHANGED,
            );
        }
        self.ex_style = unsafe { GetWindowLongA(hwnd, GWL_EXSTYLE) };
    }
}
