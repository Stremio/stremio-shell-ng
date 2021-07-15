use native_windows_gui::{self as nwg, PartialUi};
use once_cell::unsync::OnceCell;
use serde::{Deserialize, Serialize};
use serde_json;
use std::mem;
use std::rc::Rc;
use webview2::Controller;
use winapi::shared::windef::HWND__;
use winapi::um::winuser::*;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct RPCRequest {
    id: u64,
    args: Option<Vec<serde_json::Value>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct RPCResponseDataTransport {
    properties: Vec<Vec<String>>,
    signals: Vec<String>,
    methods: Vec<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct RPCResponseData {
    transport: RPCResponseDataTransport,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct RPCResponse {
    id: u64,
    object: String,
    #[serde(rename = "type")]
    response_type: u32,
    data: RPCResponseData,
}

#[derive(Default)]
pub struct WebView {
    controller: Rc<OnceCell<Controller>>,
}

impl WebView {
    fn resize_to_window_bounds_and_show(
        controller: Option<&Controller>,
        hwnd: Option<*mut HWND__>,
    ) {
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
                        webview
                            .navigate("https://www.boyanpetrov.rip/stremio/index.html")
                            .expect("Cannot load the webUI");
                            webview
                            .add_script_to_execute_on_document_created(
                                r##"
                            window.qt={webChannelTransport:{send:window.chrome.webview.postMessage}};
                            window.chrome.webview.addEventListener('message',ev=>window.qt.webChannelTransport.onmessage(ev));
                            window.onload=()=>initShellComm();
                            "##,
                                |_| Ok(()),
                            )
                            .ok();
                        webview.add_web_message_received(|w, msg| {
                            let msg = msg.try_get_web_message_as_string()?;
                            let msg: RPCRequest = serde_json::from_str(&msg).unwrap();
                            dbg!(msg.clone());
                            if msg.id == 0 {
                                let resp: RPCResponse = RPCResponse {
                                    id: 0,
                                    object: "transport".to_string(),
                                    response_type: 3,
                                    data: RPCResponseData {
                                        transport: RPCResponseDataTransport {
                                            properties: vec![vec![], vec!["".to_string(), "shellVersion".to_string(), "".to_string(), "5.0.0".to_string()]],
                                            signals: vec![],
                                            methods: vec![vec!["onEvent".to_string(), "".to_string()]]
                                        }
                                    }
                                };
                                let resp_json = serde_json::to_string(&resp).unwrap();
                                dbg!(resp_json.clone());
                                w.post_web_message_as_string(&resp_json).ok();
                            }
                            Ok(())
                        }).ok();
                        WebView::resize_to_window_bounds_and_show(Some(&controller), parent.hwnd());
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
                WebView::resize_to_window_bounds_and_show(self.controller.get(), handle.hwnd());
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
