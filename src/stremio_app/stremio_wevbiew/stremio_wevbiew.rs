use native_windows_gui::{self as nwg, PartialUi};
use once_cell::unsync::OnceCell;
use std::mem;
use std::rc::Rc;
use webview2::Controller;
use winapi::shared::windef::HWND__;
use winapi::um::winuser::*;

#[derive(Default)]
pub struct WebView {
    controller: Rc<OnceCell<Controller>>,
}

impl WebView {
    fn resize_to_window_bounds(controller: Option<&Controller>, hwnd: Option<*mut HWND__>) {
        if let (Some(controller), Some(hwnd)) = (controller, hwnd) {
            unsafe {
                let mut rect = mem::zeroed();
                GetClientRect(hwnd, &mut rect);
                controller.put_bounds(rect).ok();
                controller.put_is_visible(true).ok();
            }
        }
    }
}

impl PartialUi for WebView {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        let parent = parent.expect("No parent window").into();
        let hwnd = parent.hwnd().expect("Cannot obtain window handle");
        let controller_clone = data.controller.clone();
        let result = webview2::EnvironmentBuilder::new()
            .with_additional_browser_arguments("--disable-gpu")
            .build(move |env| {
                env.expect("Cannot obtain webview environment")
                    .create_controller(hwnd, move |controller| {
                        let controller = controller.expect("Cannot obtain webview controller");
                        WebView::resize_to_window_bounds(Some(&controller), parent.hwnd());
                        if let Ok(controller2) = controller.get_controller2() {
                            controller2
                                .put_default_background_color(webview2_sys::Color {
                                    r: 255,
                                    g: 255,
                                    b: 255,
                                    a: 0,
                                })
                                .ok();
                        } else {
                            eprintln!("failed to get interface to controller2");
                        }
                        let webview = controller
                            .get_webview()
                            .expect("Cannot obtain webview from controller");
                        // webview.navigate("edge://gpu").expect("Cannot load the webUI");
                        webview
                            .navigate("https://www.boyanpetrov.rip/stremio/index.html")
                            .expect("Cannot load the webUI");
                        // controller.put_is_visible(true).expect("Cannot show the WebView");
                        controller_clone
                            .set(controller)
                            .expect("Cannot update the controller");
                        Ok(())
                    })
            });
        if let Err(e) = result {
            nwg::modal_fatal_message(
                &parent,
                "Failed to Create WebView2 Environment",
                &format!("{}", e),
            );
        }
        Ok(())
    }
    fn process_event<'a>(
        &self,
        evt: nwg::Event,
        _evt_data: &nwg::EventData,
        handle: nwg::ControlHandle,
    ) {
        use nwg::Event as E;
        match evt {
            E::OnResize | E::OnWindowMaximize => {
                WebView::resize_to_window_bounds(self.controller.get(), handle.hwnd());
            }
            E::OnWindowMinimize => {
                if let Some(controller) = self.controller.get() {
                    controller.put_is_visible(false).ok();
                }
            }
            _ => {}
        }
    }
}
