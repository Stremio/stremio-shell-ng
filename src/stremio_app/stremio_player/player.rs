use crate::stremio_app::ipc;
use crate::stremio_app::RPCResponse;
use native_windows_gui::{self as nwg, PartialUi};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::stremio_app::stremio_player::communication::{
    PlayerEnded, PlayerEvent, PlayerProprChange, PlayerResponse,
};

#[derive(Default)]
pub struct Player {
    pub channel: ipc::Channel,
    message_queue: Arc<Mutex<VecDeque<String>>>,
}

impl PartialUi for Player {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        let (tx, rx) = mpsc::channel::<String>();
        let (tx1, rx1) = mpsc::channel::<String>();
        data.channel = RefCell::new(Some((tx, Arc::new(Mutex::new(rx1)))));
        let hwnd = parent
            .expect("No parent window")
            .into()
            .hwnd()
            .expect("Cannot obtain window handle") as i64;
        let message = data.message_queue.clone();
        thread::spawn(move || {
            let mut mpv_builder =
                mpv::MpvHandlerBuilder::new().expect("Error while creating MPV builder");
            mpv_builder
                .set_option("wid", hwnd)
                .expect("failed setting wid");
            // mpv_builder.set_option("vo", "gpu").expect("unable to set vo");
            // win, opengl: works but least performancy, 10-15% CPU
            // winvk, vulkan: works as good as d3d11
            // d3d11, d1d11: works great
            // dxinterop, auto: works, slightly more cpu use than d3d11
            // default (auto) seems to be d3d11 (vo/gpu/d3d11)
            mpv_builder
                .set_option("gpu-context", "angle")
                .and_then(|_| mpv_builder.set_option("gpu-api", "auto"))
                .expect("setting gpu options failed");
            mpv_builder
                .try_hardware_decoding()
                .expect("failed setting hwdec");
            mpv_builder
                .set_option("terminal", "yes")
                .expect("failed setting terminal");
            mpv_builder
                .set_option("msg-level", "all=v")
                .expect("failed setting msg-level");
            mpv_builder
                .set_option("quiet", "yes")
                .expect("failed setting msg-level");
            let mut mpv = mpv_builder.build().expect("Cannot build MPV");

            let thread_messages = Arc::clone(&message);

            thread::spawn(move || loop {
                if let Ok(msg) = rx.recv() {
                    let mut messages = thread_messages.lock().unwrap();
                    messages.push_back(msg);
                }
            });

            'main: loop {
                // wait up to X seconds for an event.
                while let Some(event) = mpv.wait_event(0.03) {
                    // even if you don't do anything with the events, it is still necessary to empty
                    // the event loop

                    let resp_event = match event {
                        mpv::Event::PropertyChange {
                            name,
                            change,
                            reply_userdata: _,
                        } => PlayerResponse(
                            "mpv-prop-change",
                            PlayerEvent::PropChange(PlayerProprChange::from_name_value(
                                name.to_string(),
                                change,
                            )),
                        )
                        .to_value(),
                        mpv::Event::EndFile(Ok(reason)) => PlayerResponse(
                            "mpv-event-ended",
                            PlayerEvent::End(PlayerEnded::from_end_reason(reason)),
                        )
                        .to_value(),
                        mpv::Event::Shutdown => {
                            break 'main;
                        }
                        _ => None,
                    };
                    if resp_event.is_some() {
                        tx1.send(RPCResponse::response_message(resp_event)).ok();
                    }
                } // event processing

                thread::sleep(std::time::Duration::from_millis(30));
                let mut in_message = message.lock().unwrap();
                for msg in in_message.drain(..) {
                    let (command, data): (String, serde_json::Value) =
                        serde_json::from_str(msg.as_str()).unwrap();
                    match command.as_str() {
                        "mpv-observe-prop" => {
                            let property = data.as_str().unwrap_or_default();
                            match property {
                                "pause" | "paused-for-cache" | "seeking" | "eof-reached" => {
                                    mpv.observe_property::<bool>(property, 0).ok();
                                }
                                "aid" | "vid" | "sid" => {
                                    mpv.observe_property::<i64>(property, 0).ok();
                                }
                                "time-pos"
                                | "volume"
                                | "duration"
                                | "sub-scale"
                                | "cache-buffering-state"
                                | "sub-pos" => {
                                    mpv.observe_property::<f64>(property, 0).ok();
                                }
                                "path" | "mpv-version" | "ffmpeg-version" | "track-list"
                                | "video-params" | "metadata" => {
                                    mpv.observe_property::<&str>(property, 0).ok();
                                }
                                other => {
                                    eprintln!("mpv-observe-prop: not implemented for `{}`", other);
                                }
                            };
                        }
                        "mpv-set-prop" => {
                            match serde_json::from_value::<Vec<serde_json::Value>>(data.clone()) {
                                Ok(prop_vector) if prop_vector.len() == 2 => {
                                    let prop =
                                        prop_vector[0].as_str().expect("Property is not a string");
                                    let val = prop_vector[1].clone();
                                    // If we change vo MPV panics
                                    if prop != "vo" {
                                        match val {
                                            serde_json::Value::Bool(v) => {
                                                mpv.set_property(prop, v).ok();
                                            }
                                            serde_json::Value::Number(v) => {
                                                mpv.set_property(prop, v.as_f64().unwrap()).ok();
                                            }
                                            serde_json::Value::String(v) => {
                                                mpv.set_property(prop, v.as_str()).ok();
                                            }
                                            val => eprintln!(
                                                "mpv-set-prop unsupported value {:?} for: {}",
                                                val, prop
                                            ),
                                        };
                                    };
                                }
                                Ok(prop_vector) => {
                                    eprintln!("mpv-set-prop not implemented for: {:?}", prop_vector)
                                }
                                Err(e) => {
                                    eprintln!("mpv-set-prop Error: {:?} for data {}", e, data)
                                }
                            };
                        }
                        "mpv-command" => {
                            match serde_json::from_value::<Vec<String>>(data.clone()) {
                                Ok(data) if data.len() > 0 => {
                                    let data: Vec<_> = data.iter().map(|s| s.as_str()).collect();
                                    if data[0] != "run" {
                                        mpv.command(&data).ok();
                                    }
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    eprintln!("mpv-command Error: {:?} for data {}", e, data)
                                }
                            }
                        }
                        _ => {}
                    };
                } // incoming message drain loop
            } // main loop
        });

        Ok(())
    }
}
