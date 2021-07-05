use native_windows_gui::{self as nwg, Window};
use once_cell::unsync::OnceCell;
use std::mem;
use std::rc::Rc;
use webview2::Controller;
use winapi::um::winuser::*;

fn main() {
    // native-windows-gui has some basic high DPI support with the high-dpi
    // feature. It supports the "System DPI Awareness" mode, but not the more
    // advanced Per-Monitor (v2) DPI Awareness modes.
    //
    // Use an application manifest to get rid of this deprecated warning.
    #[allow(deprecated)]
    unsafe { nwg::set_dpi_awareness() };

    nwg::init().unwrap();

    let mut window = Window::default();

    Window::builder()
        .title("Stremio")
        .size((1600, 900))
        .build(&mut window)
        .unwrap();

    let window_handle = window.handle;
    let hwnd = window_handle.hwnd().expect("unable to obtain hwnd");

    // Initialize mpv
    let mut mpv_builder = mpv::MpvHandlerBuilder::new()
        .expect("Error while creating MPV builder");
    mpv_builder.set_option("wid", hwnd as i64).expect("failed setting wid");
    //mpv_builder.set_option("vo", "gpu").expect("unable to set vo");
    // win, opengl: works but least performancy, 10-15% CPU
    // winvk, vulkan: works as good as d3d11
    // d3d11, d1d11: works great
    // dxinterop, auto: works, slightly more cpu use than d3d11
    // default (auto) seems to be d3d11 (vo/gpu/d3d11)
    /*
    mpv_builder.set_option("gpu-context", "angle")
        .and_then(|_| mpv_builder.set_option("gpu-api", "auto"))
        .expect("setting gpu options failed");
    */
    mpv_builder.try_hardware_decoding()
        .expect("failed setting hwdec");
    mpv_builder.set_option("terminal", "yes").expect("failed setting terminal");
    mpv_builder.set_option("msg-level", "all=v").expect("failed setting msg-level");
    //mpv_builder.set_option("quiet", "yes").expect("failed setting msg-level");
    let mut mpv = mpv_builder
        .build()
        .expect("Error while initializing MPV with opengl");
    //let video_path = "/home/ivo/storage/bbb_sunflower_1080p_30fps_normal.mp4";
    let video_path = "http://distribution.bbb3d.renderfarming.net/video/mp4/bbb_sunflower_1080p_30fps_normal.mp4";
    mpv.command(&["loadfile", video_path])
        .expect("Error loading file");        

    let controller: Rc<OnceCell<Controller>> = Rc::new(OnceCell::new());
    let controller_clone = controller.clone();
    let result = webview2::Environment::builder().build(move |env| {
        env.unwrap()
            .create_controller(hwnd, move |c| {
                let c = c.unwrap();

                if let Ok(c2) = c.get_controller2() {
                    c2.put_default_background_color(webview2_sys::Color {
                        r: 255,
                        g: 255,
                        b: 255,
                        a: 0,
                    })
                    .unwrap();
                } else {
                    eprintln!("failed to get interface to controller2");
                }

                unsafe {
                    let mut rect = mem::zeroed();
                    GetClientRect(hwnd, &mut rect);
                    c.put_bounds(rect).unwrap();
                }

                let webview = c.get_webview().unwrap();
                webview.navigate("https://www.boyanpetrov.rip/stremio/index.html").unwrap();

                controller_clone.set(c).unwrap();
                Ok(())
            })
    });
    if let Err(e) = result {
        nwg::modal_fatal_message(
            &window_handle,
            "Failed to Create WebView2 Environment",
            &format!("{}", e),
        );
    }
    let window_handle = window.handle;

    // There isn't an OnWindowRestored event for SC_RESTORE in
    // native-windows-gui, so we use raw events.
    nwg::bind_raw_event_handler(&window_handle, 0xffff + 1, move |_, msg, w, _| {
        match (msg, w as usize) {
            (WM_SIZE, _) => {
                if let Some(controller) = controller.get() {
                    unsafe {
                        let mut rect = mem::zeroed();
                        GetClientRect(window_handle.hwnd().unwrap(), &mut rect);
                        controller.put_bounds(rect).unwrap();
                    }
                }
            }
            (WM_MOVE, _) => {
                if let Some(controller) = controller.get() {
                    controller.notify_parent_window_position_changed().unwrap();
                }
            }
            (WM_SYSCOMMAND, SC_MINIMIZE) => {
                if let Some(controller) = controller.get() {
                    controller.put_is_visible(false).unwrap();
                }
            }
            (WM_SYSCOMMAND, SC_RESTORE) => {
                if let Some(controller) = controller.get() {
                    controller.put_is_visible(true).unwrap();
                }
            }
            (WM_CLOSE, _) => nwg::stop_thread_dispatch(),
            _ => {}
        }
        None
    })
    .unwrap();

    nwg::dispatch_thread_events();
}
