use native_windows_gui::{self as nwg, PartialUi};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use crate::stremio_app::ipc;

#[derive(Default, Serialize, Deserialize, Debug, Clone)]
pub struct MpvEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl MpvEvent {
    fn value_from_format(data: mpv::Format, as_json: bool) -> serde_json::Value {
        match data {
            mpv::Format::Flag(d) => serde_json::Value::Bool(d),
            mpv::Format::Int(d) => serde_json::Value::Number(
                serde_json::Number::from_f64(d as f64).expect("MPV returned invalid number"),
            ),
            mpv::Format::Double(d) => serde_json::Value::Number(
                serde_json::Number::from_f64(d).expect("MPV returned invalid number"),
            ),
            mpv::Format::OsdStr(s) => serde_json::Value::String(s.to_string()),
            mpv::Format::Str(s) => {
                if as_json {
                    serde_json::from_str(s).expect("MPV returned invalid JSON data")
                } else {
                    serde_json::Value::String(s.to_string())
                }
            }
        }
    }
    fn string_from_end_reason(data: mpv::EndFileReason) -> String {
        match data {
            mpv::EndFileReason::MPV_END_FILE_REASON_ERROR => "error".to_string(),
            mpv::EndFileReason::MPV_END_FILE_REASON_QUIT => "quit".to_string(),
            _ => "other".to_string(),
        }
    }
}

#[derive(Default)]
pub struct Player {
    pub channel: ipc::Channel,
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
            //mpv_builder.set_option("quiet", "yes").expect("failed setting msg-level");
            let mut mpv = mpv_builder.build().expect("Cannot build MPV");

            let message_queue: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
            let thread_messages = Arc::clone(&message_queue);

            thread::spawn(move || loop {
                if let Ok(msg) = rx.recv() {
                    let mut messages = thread_messages.lock().unwrap();
                    messages.push(msg);
                }
            });

            'main: loop {
                // wait up to X seconds for an event.
                while let Some(event) = mpv.wait_event(0.03) {
                    // even if you don't do anything with the events, it is still necessary to empty
                    // the event loop

                    let json_responses = ["track-list", "video-params", "metadata"];
                    let resp_event = match event {
                        mpv::Event::PropertyChange {
                            name,
                            change,
                            reply_userdata: _,
                        } => Some((
                            "mpv-prop-change",
                            MpvEvent {
                                name: Some(name.to_string()),
                                data: Some(MpvEvent::value_from_format(
                                    change,
                                    json_responses.contains(&name),
                                )),
                                ..Default::default()
                            },
                        )),
                        mpv::Event::EndFile(Ok(reason)) => Some((
                            "mpv-event-ended",
                            MpvEvent {
                                reason: Some(MpvEvent::string_from_end_reason(reason)),
                                ..Default::default()
                            },
                        )),
                        mpv::Event::Shutdown => {
                            break 'main;
                        }
                        _ => None,
                    };
                    if let Some(resp) = resp_event {
                        tx1.send(
                            serde_json::to_string(&resp).expect("Cannot generate MPV event JSON"),
                        )
                        .ok();
                    }
                } // event processing

                let mut in_message = message_queue.lock().unwrap();
                for msg in in_message.iter() {
                    let (command, data): (String, serde_json::Value) =
                        serde_json::from_str(msg).unwrap();
                    match command.as_str() {
                        "mpv-observe-prop" => {
                            if let Some(property) = data.as_str() {
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
                                        eprintln!(
                                            "mpv-observe-prop: not implemented for `{}`",
                                            other
                                        );
                                    }
                                };
                            }
                        }
                        "mpv-set-prop" => {
                            match serde_json::from_value::<Vec<serde_json::Value>>(data.clone()) {
                                Ok(prop_vector) => {
                                    if let [prop, val] = &prop_vector[..] {
                                        let prop = prop.as_str().expect("Property is not a string");
                                        // If we change vo MPV panics
                                        if prop != "vo" {
                                            match val {
                                                serde_json::Value::Bool(v) => {
                                                    mpv.set_property(prop, *v).ok();
                                                }
                                                serde_json::Value::Number(v) => {
                                                    mpv.set_property(prop, v.as_f64().unwrap())
                                                        .ok();
                                                }
                                                serde_json::Value::String(v) => {
                                                    mpv.set_property(prop, v.as_str()).ok();
                                                }
                                                _ => {}
                                            };
                                        };
                                    }
                                }
                                Err(e) => {
                                    eprintln!("mpv-set-prop Error: {:?} for data {}", e, data)
                                }
                            };
                        }
                        "mpv-command" => {
                            match serde_json::from_value::<Vec<String>>(data.clone()) {
                                Ok(data) => {
                                    let data: Vec<_> = data.iter().map(|s| s.as_str()).collect();
                                    mpv.command(&data).ok();
                                }
                                Err(e) => {
                                    eprintln!("mpv-command Error: {:?} for data {}", e, data)
                                }
                            }
                        }
                        _ => {}
                    };
                    // let video_path = "http://distribution.bbb3d.renderfarming.net/video/mp4/bbb_sunflower_1080p_30fps_normal.mp4";
                    // mpv.command(&["loadfile", video_path]).ok();
                    // mpv.command(&["stop"]).ok();
                    // mpv.set_property("paused", true).ok();
                }
                *in_message = vec![];
            }
        });

        Ok(())
    }
}
