use native_windows_gui::{self as nwg, PartialUi};
use once_cell::unsync::OnceCell;
use serde_json::json;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::mem;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use urlencoding::decode;
use webview2::Controller;
use winapi::shared::windef::HWND__;
use winapi::um::winuser::*;
use crate::stremio_app::ipc;

#[derive(Default)]
pub struct WebView {
    pub endpoint: Rc<OnceCell<String>>,
    controller: Rc<OnceCell<Controller>>,
    pub channel: ipc::Channel,
    notice: nwg::Notice,
    compute: RefCell<Option<thread::JoinHandle<()>>>,
    message_queue: Arc<Mutex<VecDeque<String>>>,
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
            }
            controller.put_is_visible(true).ok();
            controller
                .move_focus(webview2::MoveFocusReason::Programmatic)
                .ok();
        }
    }
}

impl PartialUi for WebView {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        let (tx, rx) = mpsc::channel::<String>();
        let tx_drag_drop = tx.clone();
        let (tx_web, rx_web) = mpsc::channel::<String>();
        data.channel = RefCell::new(Some((tx, Arc::new(Mutex::new(rx_web)))));

        let parent = parent.expect("No parent window").into();

        let hwnd = parent.hwnd().expect("Cannot obtain window handle") as i64;
        nwg::Notice::builder()
            .parent(parent)
            .build(&mut data.notice)
            .ok();
        let controller_clone = data.controller.clone();
        let endpoint = data.endpoint.clone();
        let hwnd = hwnd as *mut HWND__;
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
                    if let Some(endpoint) = endpoint.get() {
                        if webview
                            .navigate(endpoint.as_str()).is_err() {
                                tx_web.clone().send(format!(r#"{{"id":1,"args":["app-error","Cannot load WEB UI at '{}'"]}}"#, &endpoint)).ok();
                        };
                    }
                        webview
                            .add_script_to_execute_on_document_created(
                                r##"
                            window.qt={webChannelTransport:{send:window.chrome.webview.postMessage}};
                            window.chrome.webview.addEventListener('message',ev=>window.qt.webChannelTransport.onmessage(ev));
                            window.onload=()=>{try{initShellComm();}catch(e){window.chrome.webview.postMessage('{"id":1,"args":["app-error","'+e.message+'"]}')}};
                            "##,
                                |_| Ok(()),
                            )
                            .ok();
                        webview.add_web_message_received(move |_w, msg| {
                            let msg = msg.try_get_web_message_as_string()?;
                            tx_web.send(msg).ok();
                            Ok(())
                        }).ok();
                        webview.add_new_window_requested(move |_w, msg| {
                            if let Some(file) = msg.get_uri().ok().and_then(|str| {decode(str.as_str()).ok().map(Cow::into_owned)}) {
                                let data = json!({
                                    "object": "transport",
                                    "type": 1,
                                    "args": ["dragdrop" ,[file]]
                                });
                                tx_drag_drop.send(data.to_string()).ok();
                                msg.put_handled(true).ok();
                            }
                            Ok(())
                        }).ok();
                        WebView::resize_to_window_bounds_and_show(Some(&controller), Some(hwnd));
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

        let sender = data.notice.sender();
        let message = data.message_queue.clone();
        *data.compute.borrow_mut() = Some(thread::spawn(move || loop {
            if let Ok(msg) = rx.recv() {
                let mut message = message.lock().unwrap();
                message.push_back(msg);
                sender.notice();
            }
        }));

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
            // FIXME: Hack to focus the webview when pressing alt-tab.
            // This doesn't work if you click the window's title with
            // the mouse. A better solution is needed
            E::OnKeyPress | E::OnKeyRelease => {
                if let Some(controller) = self.controller.get() {
                    controller
                        .move_focus(webview2::MoveFocusReason::Programmatic)
                        .ok();
                }
            }
            E::OnResize | E::OnWindowMaximize => {
                WebView::resize_to_window_bounds_and_show(self.controller.get(), handle.hwnd());
            }
            E::OnWindowMinimize => {
                if let Some(controller) = self.controller.get() {
                    controller.put_is_visible(false).ok();
                }
            }
            E::OnNotice => {
                let message_queue = self.message_queue.clone();
                if let Some(controller) = self.controller.get() {
                    let webview = controller.get_webview().expect("Cannot get vebview");
                    let mut message_queue = message_queue.lock().unwrap();
                    for msg in message_queue.drain(..) {
                        webview.post_web_message_as_string(msg.as_str()).ok();
                    }
                }
            }
            _ => {}
        }
    }
}
