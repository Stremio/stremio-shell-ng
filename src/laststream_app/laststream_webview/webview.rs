use crate::laststream_app::constants::{safe_url, SERVER_IPC_KEY};
use crate::laststream_app::ipc;
use crate::laststream_app::robust::{self, OrReport};
use native_windows_gui::{self as nwg, PartialUi};
use once_cell::unsync::OnceCell;
use serde_json::json;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::mem;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use urlencoding::decode;
use webview2::{check_hresult, Controller};
use webview2_sys::ICoreWebView2Settings3;
use winapi::shared::windef::HWND;
use winapi::um::winuser::{
    GetClientRect, GetKeyState, VK_CONTROL, VK_F7, VK_LEFT, VK_RIGHT, WM_APPCOMMAND, WM_SETFOCUS,
};

const SEEK_KEY_REPEAT_INTERVAL_MS: u128 = 150;
const APPCOMMAND_MEDIA_NEXTTRACK: u32 = 11;
const APPCOMMAND_MEDIA_PREVIOUSTRACK: u32 = 12;
const APPCOMMAND_MEDIA_PLAY_PAUSE: u32 = 14;
const APPCOMMAND_MEDIA_PLAY: u32 = 46;
const APPCOMMAND_MEDIA_PAUSE: u32 = 47;
const VK_F: u32 = b'F' as u32;

#[derive(Default)]
pub struct WebView {
    endpoint: Rc<RefCell<Option<String>>>,
    pub dev_tools: Rc<OnceCell<bool>>,
    pub controller: Rc<OnceCell<Controller>>,
    pub channel: ipc::Channel,
    notice: nwg::Notice,
    message_queue: Arc<Mutex<VecDeque<String>>>,
}

impl WebView {
    pub fn navigate(&self, endpoint: String) -> webview2::Result<()> {
        *self.endpoint.borrow_mut() = Some(endpoint.clone());
        if let Some(controller) = self.controller.get() {
            Self::navigate_webview(&controller.get_webview()?, &endpoint)?;
        }
        Ok(())
    }

    fn navigate_webview(webview: &webview2::WebView, endpoint: &str) -> webview2::Result<()> {
        let source = webview.get_source()?;
        if source.split('#').next() == endpoint.split('#').next() {
            // A fragment-only navigation would leave Core connected to the failed server.
            // Set the new endpoint before reloading, including when the port is unchanged.
            let fragment = endpoint.split_once('#').map(|(_, hash)| hash).unwrap_or("");
            webview.execute_script(
                &format!(
                    "window.location.hash = {}; window.location.reload();",
                    json!(fragment)
                ),
                |_| Ok(()),
            )
        } else {
            webview.navigate(endpoint)
        }
    }

    pub fn set_interface_scale(&self, scale: u64) {
        if let Some(controller) = self.controller.get() {
            if let Err(error) = controller.put_zoom_factor(scale as f64 / 100.0) {
                eprintln!("Cannot set interface scale: {error}");
            }
        }
    }

    pub fn fit_to_window(&self, hwnd: Option<HWND>) {
        if let Some(hwnd) = hwnd {
            unsafe {
                let mut rect = mem::zeroed();
                GetClientRect(hwnd, &mut rect);
                self.controller
                    .get()
                    .and_then(|controller| controller.put_bounds(rect).ok());
            }
        }
    }

    fn resize_to_window_bounds(controller: Option<&Controller>, hwnd: Option<HWND>) {
        if let (Some(controller), Some(hwnd)) = (controller, hwnd) {
            unsafe {
                let mut rect = mem::zeroed();
                GetClientRect(hwnd, &mut rect);
                controller.put_bounds(rect).ok();
            }
        }
    }
}

impl PartialUi for WebView {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        println!("Building WebView");
        let (tx, rx) = flume::unbounded();
        let tx_drag_drop = tx.clone();
        let tx_media = tx.clone();
        let (tx_web, rx_web) = flume::unbounded();
        let tx_fs = tx_web.clone();
        data.channel = RefCell::new(Some((tx, rx_web)));

        let Some(parent) = parent.map(|parent| -> nwg::ControlHandle { parent.into() }) else {
            return Err(nwg::NwgError::initialization(
                "WebView: pas de fenêtre parente",
            ));
        };

        let Some(hwnd) = parent.hwnd() else {
            return Err(nwg::NwgError::initialization(
                "WebView: poignée de fenêtre introuvable",
            ));
        };
        nwg::Notice::builder()
            .parent(parent)
            .build(&mut data.notice)
            .ok();
        let controller_clone = data.controller.clone();
        let endpoint = data.endpoint.clone();
        let dev_tools_flag = data.dev_tools.clone();
        let webview_flags = "--autoplay-policy=no-user-gesture-required --disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection";
        let result = webview2::EnvironmentBuilder::new()
            .with_additional_browser_arguments(webview_flags)
            .build(move |env| {
                env.or_report("WebView2: environnement indisponible")?
                    .create_controller(hwnd, move |controller| {
                        let controller = controller.or_report("WebView2: contrôleur indisponible")?;
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
                            .or_report("WebView2: vue indisponible")?;
                    let settings = webview
                        .get_settings()
                        .or_report("WebView2: réglages indisponibles")?;
                    settings.put_is_status_bar_enabled(false).ok();
                    let dev_tools = devtools_allowed(
                        cfg!(debug_assertions),
                        dev_tools_flag.get().copied().unwrap_or(false),
                    );
                    settings.put_are_dev_tools_enabled(dev_tools).ok();
                    settings.put_are_default_context_menus_enabled(true).ok();
                    // Web handles zoom shortcuts so they update the saved interface scale.
                    settings.put_is_zoom_control_enabled(false).ok();
                    settings.put_is_built_in_error_page_enabled(false).ok();
                    settings.put_are_host_objects_allowed(false).ok();
                    settings.put_are_default_script_dialogs_enabled(false).ok();
                    if let Some(settings3) = settings
                        .as_inner()
                        .get_interface::<dyn ICoreWebView2Settings3>()
                    {
                        check_hresult(unsafe {
                            settings3.put_are_browser_accelerator_keys_enabled(0)
                        })
                        .ok();
                    }

                    // Handle window.open and href
                    webview.add_new_window_requested(move |_webview, event| {
                        if let Ok(uri) = event.get_uri() {
                            if let Some(final_url) = safe_url(&uri) {
                                if let Err(e) = open::that(final_url) {
                                    eprintln!("Failed to open URL: {e}");
                                }
                            }
                        }

                        Ok(())
                    })?;

                    let navigation_endpoint = endpoint.clone();
                    webview.add_navigation_starting(move |_webview, event| {
                        let uri = event.get_uri()?;
                        if !same_origin(&trusted_origin(&navigation_endpoint), &uri) {
                            event.put_cancel(true)?;
                            if let Some(final_url) = safe_url(&uri) {
                                if let Err(e) = open::that(final_url) {
                                    eprintln!("Failed to open URL: {e}");
                                }
                            }
                        }
                        Ok(())
                    })?;

                    let navigation_tx = tx_web.clone();
                    let message_endpoint = endpoint.clone();
                        webview.add_web_message_received(move |_w, msg| {
                            let source = msg.get_source()?;
                            if !same_origin(&trusted_origin(&message_endpoint), &source) {
                                eprintln!("Ignored web message from {source}");
                                return Ok(());
                            }
                            let msg = msg.try_get_web_message_as_string()?;
                            tx_web.send(msg).ok();
                            Ok(())
                        }).or_report("WebView2: message web non branché")?;
                        webview.add_new_window_requested(move |_w, msg| {
                            if let Some(file) = msg.get_uri().ok().and_then(|str| {decode(str.as_str()).ok().map(Cow::into_owned)}) {
                                tx_drag_drop.send(ipc::RPCResponse::response_message(Some(json!(["dragdrop" ,[file]])))).ok();
                                msg.put_handled(true).ok();
                            }
                            Ok(())
                        }).or_report("WebView2: glisser-déposer non branché")?;
                        webview.add_contains_full_screen_element_changed(move |wv| {
                            if let Ok(visibility) = wv.get_contains_full_screen_element() {
                                tx_fs.send(ipc::RPCResponse::response_message(Some(json!(["win-set-visibility" , {"fullscreen": visibility}])))).ok();
                            }
                            Ok(())
                        }).or_report("WebView2: plein écran non branché")?;

                        webview.add_content_loading(move |wv, _| {
                            let key = std::env::var(SERVER_IPC_KEY).unwrap_or_default();
                            wv.execute_script(&server_ipc_key_script(&key), |_| Ok(()))
                                .or_report("WebView2: clé IPC non injectée")?;

                            wv.execute_script(r##"
                            window.addEventListener('contextmenu', event => {
                                if (event.shiftKey) event.stopImmediatePropagation();
                            }, true);
                            window.addEventListener('mouseup', event => {
                                if (event.shiftKey && event.button === 2) {
                                    event.stopImmediatePropagation();
                                }
                            }, true);
                            try{console.log('Shell JS injected');if(window.self === window.top) {
                                window.qt={webChannelTransport:{send:window.chrome.webview.postMessage}};
                                window.chrome.webview.addEventListener('message',ev=>window.qt.webChannelTransport.onmessage(ev));
                                document.addEventListener('click', event => {
                                    const link = event.target.closest('a[href]');
                                    const href = link && link.getAttribute('href');
                                    if (href && (href.startsWith('data:application/octet-stream;charset=utf-8;base64,') || /^vlc:/i.test(href))) {
                                        event.preventDefault();
                                        window.chrome.webview.postMessage(JSON.stringify({id: 1, args: ['play-external', href]}));
                                    }
                                }, true);
                                }}catch(e){}
                            window.addEventListener("load", function() {if(initShellComm) try { initShellComm() } catch(e) {}}, false)

                            "##, |_| Ok(())).or_report("WebView2: script du shell non injecté")?;
                            Ok(())
                        }).or_report("WebView2: chargement de contenu non branché")?;

                        WebView::resize_to_window_bounds(Some(&controller), Some(hwnd));
                        controller.put_is_visible(true).ok();
                        controller
                            .move_focus(webview2::MoveFocusReason::Programmatic)
                            .ok();
                        let last_seek_repeat: RefCell<HashMap<u32, Instant>> =
                            RefCell::new(HashMap::new());
                        controller.add_accelerator_key_pressed(move |_, e| {
                            let k = e.get_virtual_key()?;
                            let ctrl_pressed =
                                unsafe { (GetKeyState(VK_CONTROL) as u16 & 0x8000) != 0 };
                            if k == VK_F7 as u32 || (k == VK_F && ctrl_pressed) {
                                return e.put_handled(true);
                            }

                            if k != VK_LEFT as u32 && k != VK_RIGHT as u32 {
                                return Ok(());
                            }

                            let status = e.get_physical_key_status()?;
                            if status.is_key_released != 0 {
                                last_seek_repeat.borrow_mut().remove(&k);
                                return Ok(());
                            }

                            if status.was_key_down != 0 {
                                let now = Instant::now();
                                let mut repeats = last_seek_repeat.borrow_mut();
                                if let Some(last) = repeats.get(&k) {
                                    if now.duration_since(*last).as_millis()
                                        < SEEK_KEY_REPEAT_INTERVAL_MS
                                    {
                                        return e.put_handled(true);
                                    }
                                }
                                repeats.insert(k, now);
                            }

                            Ok(())
                        })
                        .or_report("WebView2: raccourcis clavier non branchés")?;

                        let navigation = endpoint.borrow().clone();
                        if controller_clone.set(controller).is_err() {
                            robust::report_error(
                                "WebView2",
                                &"contrôleur déjà enregistré, le nouveau est ignoré",
                            );
                        }
                        if let Some(endpoint) = navigation {
                            if WebView::navigate_webview(&webview, &endpoint).is_err() {
                                navigation_tx.send(ipc::RPCResponse::response_message(Some(json!(["app-error", format!("Cannot load WEB UI at '{}'", &endpoint)])))).ok();
                            }
                        }
                        Ok(())
                    })
            });
        if let Err(e) = result {
            nwg::modal_fatal_message(
                parent,
                "Failed to Create WebView2 Environment",
                &format!("{e}"),
            );
        }

        let sender = data.notice.sender();
        let message = data.message_queue.clone();
        robust::SERVICES.spawn("webview-messages", move || {
            while let Some(msg) = robust::SERVICES.recv(&rx) {
                robust::lock(&message, "file des messages WebView").push_back(msg);
                sender.notice();
            }
        });

        // handler ids equal or smaller than 0xFFFF are reserved by NWG
        let handler_id = 0x10000;
        let controller_clone = data.controller.clone();
        nwg::bind_raw_event_handler(&parent, handler_id, move |_hwnd, msg, _w, l| {
            if msg == WM_SETFOCUS {
                controller_clone.get().and_then(|controller| {
                    controller
                        .move_focus(webview2::MoveFocusReason::Programmatic)
                        .ok()
                });
            } else if msg == WM_APPCOMMAND {
                let cmd = ((l >> 16) & 0xFFF) as u32;
                let action = match cmd {
                    APPCOMMAND_MEDIA_PLAY_PAUSE
                    | APPCOMMAND_MEDIA_PLAY
                    | APPCOMMAND_MEDIA_PAUSE => Some("play-pause"),
                    APPCOMMAND_MEDIA_NEXTTRACK => Some("next-track"),
                    APPCOMMAND_MEDIA_PREVIOUSTRACK => Some("previous-track"),
                    _ => None,
                };
                if let Some(action) = action {
                    tx_media.send(ipc::RPCResponse::media_key(action)).ok();
                    return Some(1);
                }
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
        if evt == E::OnNotice && handle == self.notice.handle {
            let message_queue = self.message_queue.clone();
            if let Some(controller) = self.controller.get() {
                let webview = match controller.get_webview() {
                    Ok(webview) => webview,
                    Err(error) => {
                        robust::report_error(
                            "WebView2: vue indisponible pour les messages",
                            &error,
                        );
                        return;
                    }
                };
                let mut message_queue = robust::lock(&message_queue, "file des messages WebView");
                for msg in message_queue.drain(..) {
                    webview.post_web_message_as_string(msg.as_str()).ok();
                }
            }
        }
    }
}

fn trusted_origin(endpoint: &RefCell<Option<String>>) -> Option<url::Origin> {
    endpoint
        .borrow()
        .as_deref()
        .and_then(|endpoint| url::Url::parse(endpoint).ok())
        .map(|endpoint| endpoint.origin())
}

fn same_origin(trusted: &Option<url::Origin>, uri: &str) -> bool {
    match trusted {
        Some(trusted) => url::Url::parse(uri).is_ok_and(|url| url.origin() == *trusted),
        None => false,
    }
}

/// Script qui fixe la clé IPC du serveur dans la page. La clé passe par serde_json (guillemets, antislashs,
/// caractères de contrôle). `<`, `>` et les séparateurs Unicode sont ensuite écrits en échappements `\u`, pour
/// qu'aucune valeur ne puisse fermer une balise `</script>` si ce script venait à être inséré dans du HTML.
fn server_ipc_key_script(key: &str) -> String {
    let literal = json!(key)
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    format!("window.laststream_server_ipc_key = {literal};")
}

/// Les outils de développement sont toujours disponibles dans un build de débogage ; en release, seulement
/// avec l'option `--devtools` (ou `--development`, voir main.rs).
fn devtools_allowed(debug_build: bool, devtools_flag: bool) -> bool {
    debug_build || devtools_flag
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::laststream_app::shell_config::PUBLIC_URL;

    fn trusted(endpoint: &str) -> Option<url::Origin> {
        trusted_origin(&RefCell::new(Some(endpoint.to_owned())))
    }

    #[test]
    fn public_mode_accepts_messages_only_from_the_public_origin() {
        let origin = trusted(PUBLIC_URL);
        assert!(same_origin(
            &origin,
            "https://laststream.gtux-prog.workers.dev/#/player"
        ));
        assert!(!same_origin(&origin, "http://127.0.0.1:7000/"));
        assert!(!same_origin(&origin, "https://evil.workers.dev/"));
        assert!(!same_origin(
            &origin,
            "http://laststream.gtux-prog.workers.dev/"
        ));
        assert!(!same_origin(
            &origin,
            "https://laststream.gtux-prog.workers.dev.evil.com/"
        ));
    }

    #[test]
    fn local_mode_accepts_only_the_local_origin() {
        let origin = trusted("http://127.0.0.1:7000/");
        assert!(same_origin(&origin, "http://127.0.0.1:7000/#/"));
        assert!(!same_origin(&origin, "http://127.0.0.1:7001/"));
        assert!(!same_origin(&origin, PUBLIC_URL));
    }

    #[test]
    fn server_ipc_key_is_injected_as_a_json_string_literal() {
        let key = "a\"b</script>\\c\u{2028}";
        let script = server_ipc_key_script(key);
        let literal = script
            .strip_prefix("window.laststream_server_ipc_key = ")
            .and_then(|rest| rest.strip_suffix(';'))
            .expect("the key is assigned to window.laststream_server_ipc_key");
        assert!(!script.contains("</script"));
        let decoded: String =
            serde_json::from_str(literal).expect("the assigned value is a JSON string");
        assert_eq!(decoded, key);
    }

    #[test]
    fn devtools_are_on_in_debug_builds_and_opt_in_otherwise() {
        assert!(devtools_allowed(true, false));
        assert!(devtools_allowed(false, true));
        assert!(!devtools_allowed(false, false));
    }

    #[test]
    fn nothing_is_accepted_before_the_first_navigation() {
        assert!(!same_origin(
            &trusted_origin(&RefCell::new(None)),
            PUBLIC_URL
        ));
    }
}
