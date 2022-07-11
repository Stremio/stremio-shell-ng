use crate::stremio_app::ipc;
use native_windows_gui::{self as nwg, PartialUi};
use once_cell::unsync::OnceCell;
use serde_json::json;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::mem;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use urlencoding::decode;
use webview2::Controller;
use winapi::shared::windef::HWND;
use winapi::um::winuser::{GetClientRect, WM_SETFOCUS};

#[derive(Default)]
pub struct WebView {
    pub endpoint: Rc<OnceCell<String>>,
    pub dev_tools: Rc<OnceCell<bool>>,
    controller: Rc<OnceCell<Controller>>,
    pub channel: ipc::Channel,
    notice: nwg::Notice,
    compute: RefCell<Option<thread::JoinHandle<()>>>,
    message_queue: Arc<Mutex<VecDeque<String>>>,
}

impl WebView {
    fn resize_to_window_bounds_and_show(controller: Option<&Controller>, hwnd: Option<HWND>) {
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
        let (tx, rx) = flume::unbounded();
        let tx_drag_drop = tx.clone();
        let (tx_web, rx_web) = flume::unbounded();
        data.channel = RefCell::new(Some((tx, rx_web)));

        let parent = parent.expect("No parent window").into();

        let hwnd = parent.hwnd().expect("Cannot obtain window handle");
        nwg::Notice::builder()
            .parent(parent)
            .build(&mut data.notice)
            .ok();
        let controller_clone = data.controller.clone();
        let endpoint = data.endpoint.clone();
        let dev_tools = data.dev_tools.clone();
        let result = webview2::EnvironmentBuilder::new()
            .with_additional_browser_arguments("--disable-web-security --disable-gpu --autoplay-policy=no-user-gesture-required")
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
                    let settings = webview.get_settings().unwrap();
                    settings.put_is_status_bar_enabled(false).ok();
                    settings.put_are_dev_tools_enabled(*dev_tools.get().unwrap()).ok();
                    settings.put_are_default_context_menus_enabled(false).ok();
                    settings.put_is_zoom_control_enabled(false).ok();
                    settings.put_is_built_in_error_page_enabled(false).ok();
                    if let Some(endpoint) = endpoint.get() {
                        if webview
                            .navigate(endpoint.as_str()).is_err() {
                                tx_web.clone().send(ipc::RPCResponse::response_message(Some(json!(["app-error", format!("Cannot load WEB UI at '{}'", &endpoint)])))).ok();
                        };
                    }
                        webview
                            .add_script_to_execute_on_document_created(
                                r##"
                            try{if(window.self === window.top) {
                            window.qt={webChannelTransport:{send:window.chrome.webview.postMessage}};
                            window.chrome.webview.addEventListener('message',ev=>window.qt.webChannelTransport.onmessage(ev));
                            window.onload=()=>{try{initShellComm();}catch(e){window.chrome.webview.postMessage('{"id":1,"args":["app-error","'+e.message+'"]}')}};
                            }}catch(e){}
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
                                tx_drag_drop.send(ipc::RPCResponse::response_message(Some(json!(["dragdrop" ,[file]])))).ok();
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

        // handler ids equal or smaller than 0xFFFF are reserved by NWG
        let handler_id = 0x10000;
        let controller_clone = data.controller.clone();
        nwg::bind_raw_event_handler(&parent, handler_id, move |_hwnd, msg, _w, _l| {
            if msg == WM_SETFOCUS {
                controller_clone.get().and_then(|controller| {
                    controller
                        .move_focus(webview2::MoveFocusReason::Programmatic)
                        .ok()
                });
            }
            None
        })
        .ok();

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
            E::OnPaint => {
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
