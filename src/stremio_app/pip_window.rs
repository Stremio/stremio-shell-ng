use native_windows_gui as nwg;
use std::{cell::Cell, rc::Rc};
use winapi::shared::windef::{HWND, RECT};
use winapi::um::winuser::{
    GetClientRect, GetWindowLongA, GetWindowRect, MoveWindow, SetParent, SetWindowLongA,
    SetWindowPos, GWL_STYLE, HWND_TOP, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_SHOWWINDOW, WM_CLOSE, WM_SIZE, WS_CHILD, WS_EX_TOPMOST, WS_POPUP,
};

use crate::stremio_app::window_settings::PipPlacement;

/// Native PiP host for the MPV child window.
///
/// The Web UI owns the PiP control. This type only owns the native window and
/// temporarily reparents MPV into it, keeping the shell implementation small.
#[derive(Default)]
pub struct PipWindow {
    pub window: nwg::Window,
    pub built: Cell<bool>,
    mpv_child: Rc<Cell<Option<HWND>>>,
    original_mpv_style: Rc<Cell<Option<i32>>>,
}

impl PipWindow {
    pub fn build(
        &mut self,
        close_sender: nwg::NoticeSender,
        initial_pos: Option<(i32, i32)>,
        initial_size: Option<(i32, i32)>,
    ) -> Result<(), nwg::NwgError> {
        if self.built.get() {
            return Ok(());
        }

        let (width, height) = initial_size.unwrap_or((640, 360));
        let mut builder = nwg::Window::builder()
            .title("Stremio - Picture-in-Picture")
            .size((width.max(320), height.max(180)))
            .flags(
                nwg::WindowFlags::WINDOW
                    | nwg::WindowFlags::RESIZABLE
                    | nwg::WindowFlags::MINIMIZE_BOX,
            )
            .ex_flags(WS_EX_TOPMOST);
        if let Some((x, y)) = initial_pos {
            builder = builder.position((x, y));
        }
        builder.build(&mut self.window)?;

        // Closing the PiP window is equivalent to pressing the web UI's exit
        // control. The main window performs the reparenting on its UI thread.
        let mpv_child = self.mpv_child.clone();
        nwg::bind_raw_event_handler(&self.window.handle, 0x10002, move |hwnd, msg, _w, _l| {
            match msg {
                WM_CLOSE => {
                    close_sender.notice();
                    return Some(0);
                }
                WM_SIZE => {
                    if let Some(child) = mpv_child.get() {
                        unsafe { resize_child_to_parent(child, hwnd) };
                    }
                }
                _ => {}
            }
            None
        })
        .ok();

        self.built.set(true);
        self.window.set_visible(false);
        Ok(())
    }

    pub fn show(&self) {
        self.window.set_visible(true);
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
    }

    pub fn attach_video(&self, child: HWND) -> bool {
        let Some(target) = self.window.handle.hwnd() else {
            return false;
        };

        unsafe {
            let style = GetWindowLongA(child, GWL_STYLE);
            self.original_mpv_style.set(Some(style));
            SetParent(child, target);
            SetWindowLongA(
                child,
                GWL_STYLE,
                (style | WS_CHILD as i32) & !(WS_POPUP as i32),
            );
            resize_child_to_parent(child, target);
            SetWindowPos(
                child,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            SetWindowPos(
                target,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
        self.mpv_child.set(Some(child));
        true
    }

    pub fn detach_video(&self, target: HWND) {
        let Some(child) = self.mpv_child.take() else {
            return;
        };

        unsafe {
            SetParent(child, target);
            if let Some(style) = self.original_mpv_style.take() {
                SetWindowLongA(child, GWL_STYLE, style);
            }
            resize_child_to_parent(child, target);
            SetWindowPos(
                child,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    pub fn current_placement(&self) -> Option<PipPlacement> {
        let hwnd = self.window.handle.hwnd()?;
        unsafe {
            let mut rect: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut rect) == 0 {
                return None;
            }
            Some(PipPlacement {
                x: rect.left,
                y: rect.top,
                width: (rect.right - rect.left).max(320),
                height: (rect.bottom - rect.top).max(180),
                transparent: false,
            })
        }
    }
}

unsafe fn resize_child_to_parent(child: HWND, parent: HWND) {
    let mut rect: RECT = std::mem::zeroed();
    if GetClientRect(parent, &mut rect) != 0 {
        MoveWindow(
            child,
            0,
            0,
            (rect.right - rect.left).max(1),
            (rect.bottom - rect.top).max(1),
            1,
        );
    }
}
