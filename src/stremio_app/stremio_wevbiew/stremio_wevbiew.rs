// mod stremio_wevbiew {
use native_windows_gui::{self as nwg, PartialUi};
use once_cell::unsync::OnceCell;
use std::mem;
use std::rc::Rc;
use webview2::Controller;
use winapi::um::winuser::*;

#[derive(Default)]
pub struct WebView {
    controller: Rc<OnceCell<Controller>>,
}
impl PartialUi for WebView {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        let parent = parent.unwrap().into();
        let hwnd = parent.hwnd().unwrap();
        let controller_clone = data.controller.clone();
        let result = webview2::EnvironmentBuilder::new()
            .with_additional_browser_arguments("--disable-gpu")
            .build(move |env| {
                env.unwrap().create_controller(hwnd, move |c| {
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
                    webview.navigate("edge://gpu").unwrap();
                    webview
                        .navigate("https://www.boyanpetrov.rip/stremio/index.html")
                        .unwrap();
                    // c.put_is_visible(true).expect("Cannot show the WebView");
                    controller_clone.set(c).unwrap();
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
                if let Some(controller) = self.controller.get() {
                    unsafe {
                        let mut rect = mem::zeroed();
                        GetClientRect(handle.hwnd().unwrap(), &mut rect);
                        controller.put_bounds(rect).unwrap();
                        controller.put_is_visible(true).unwrap();
                    }
                }
            }
            E::OnWindowMinimize => {
                if let Some(controller) = self.controller.get() {
                    controller.put_is_visible(false).unwrap();
                }
            }
            _ => {}
        }
    }
}
// }