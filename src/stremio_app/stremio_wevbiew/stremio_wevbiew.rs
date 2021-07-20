use native_windows_gui::{self as nwg, PartialUi};
use once_cell::unsync::OnceCell;
use std::cell::RefCell;
use std::mem;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use webview2::Controller;
use winapi::shared::windef::HWND__;
use winapi::um::winuser::*;

#[derive(Default)]
pub struct WebView {
    controller: Rc<OnceCell<Controller>>,
    pub channel: RefCell<Option<(mpsc::Sender<String>, Arc<Mutex<mpsc::Receiver<String>>>)>>,
    notice: nwg::Notice,
    compute: RefCell<Option<thread::JoinHandle<()>>>,
    message_queue: Arc<Mutex<Vec<String>>>,
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
        let (tx, rx) = mpsc::channel::<String>();
        let (tx1, rx1) = mpsc::channel::<String>();
        data.channel = RefCell::new(Some((tx, Arc::new(Mutex::new(rx1)))));

        let parent = parent.expect("No parent window").into();

        let hwnd = parent.hwnd().expect("Cannot obtain window handle") as i64;
        nwg::Notice::builder()
            .parent(parent)
            .build(&mut data.notice)
            .ok();
        let controller_clone = data.controller.clone();
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
                        webview
                            // .navigate("https://www.boyanpetrov.rip/stremio/index.html")
                            .navigate("http://app.strem.io/shell-v4.4/")
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
                        webview.add_web_message_received(move |_w, msg| {
                            let msg = msg.try_get_web_message_as_string()?;
                            tx1.send(msg).ok();
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
                message.push(msg);
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
            E::OnInit => {}
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
                    for msg in message_queue.iter() {
                        webview.post_web_message_as_string(msg).ok();
                    }
                    *message_queue = vec![];
                }
            }
            _ => {}
        }
    }
}
