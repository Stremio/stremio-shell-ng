use crate::stremio_app::ipc;
use crate::stremio_app::RPCResponse;
use libmpv::events::Event;
use libmpv::{Format, Mpv};
use native_windows_gui::{self as nwg, PartialUi};
use std::cell::RefCell;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::stremio_app::stremio_player::{
    InMsg, InMsgArgs, InMsgFn, PlayerEnded, PlayerEvent, PlayerProprChange, PlayerResponse,
    PropKey, PropVal,
};

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
            // builder thread
            let mpv = Mpv::with_initializer(|init| {
                init.set_property("wid", hwnd)?;
                init.set_property("gpu-context", "angle")?;
                init.set_property("gpu-api", "auto")?;
                init.set_property("title", "Stremio")?;
                init.set_property("terminal", "yes")?;
                init.set_property("msg-level", "all=no,cplayer=debug")?;
                init.set_property("quiet", "yes")?;
                Ok(())
            })
            .expect("Cannot create MPV");
            let ev_ctx = Arc::new(Mutex::new(mpv.create_event_context()));
            crossbeam_utils::thread::scope(|scope| {
                scope.spawn(|_| {
                    let rx = rx;
                    for msg in rx.iter() {
                        match serde_json::from_str::<InMsg>(msg.as_str()) {
                            Ok(InMsg(
                                InMsgFn::MpvObserveProp,
                                InMsgArgs::ObProp(PropKey::Bool(prop)),
                            )) => {
                                let ev_ctx = ev_ctx.lock().unwrap();
                                ev_ctx.observe_property(prop.to_string().as_str(), Format::Flag, 0)
                            }
                            Ok(InMsg(
                                InMsgFn::MpvObserveProp,
                                InMsgArgs::ObProp(PropKey::Int(prop)),
                            )) => {
                                let ev_ctx = ev_ctx.lock().unwrap();
                                ev_ctx.observe_property(prop.to_string().as_str(), Format::Int64, 0)
                            }
                            Ok(InMsg(
                                InMsgFn::MpvObserveProp,
                                InMsgArgs::ObProp(PropKey::Fp(prop)),
                            )) => {
                                let ev_ctx = ev_ctx.lock().unwrap();
                                ev_ctx.observe_property(
                                    prop.to_string().as_str(),
                                    Format::Double,
                                    0,
                                )
                            }
                            Ok(InMsg(
                                InMsgFn::MpvObserveProp,
                                InMsgArgs::ObProp(PropKey::Str(prop)),
                            )) => {
                                let ev_ctx = ev_ctx.lock().unwrap();
                                ev_ctx.observe_property(
                                    prop.to_string().as_str(),
                                    Format::String,
                                    0,
                                )
                            }
                            Ok(InMsg(
                                InMsgFn::MpvSetProp,
                                InMsgArgs::StProp(prop, PropVal::Bool(val)),
                            )) => mpv.set_property(prop.to_string().as_str(), val),
                            Ok(InMsg(
                                InMsgFn::MpvSetProp,
                                InMsgArgs::StProp(prop, PropVal::Num(val)),
                            )) => mpv.set_property(prop.to_string().as_str(), val),
                            Ok(InMsg(
                                InMsgFn::MpvSetProp,
                                InMsgArgs::StProp(prop, PropVal::Str(val)),
                            )) => mpv.set_property(prop.to_string().as_str(), val.as_str()),
                            Ok(InMsg(InMsgFn::MpvCommand, InMsgArgs::Cmd(cmd))) => {
                                let cmd: Vec<String> = cmd.into();
                                let cmd = cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>();
                                mpv.command(cmd[0], &cmd[1..])
                            }
                            _ => {
                                eprintln!("MPV unsupported message {}", msg);
                                Ok(())
                            }
                        }
                        .ok();
                    } // incoming message drain loop
                });

                'main: loop {
                    // Give time for observe_property commands to be processed before locking the mutex
                    thread::sleep(std::time::Duration::from_millis(30));
                    // even if you don't do anything with the events, it is still necessary to empty
                    // the event loop
                    let mut ev_ctx = ev_ctx.lock().unwrap();
                    // wait up to X seconds for an event. -1 means forever; 0 returns event isntantly
                    while let Some(event) = ev_ctx.wait_event(1.0) {
                        let resp_event = match event {
                            Ok(Event::PropertyChange { name, change, .. }) => PlayerResponse(
                                "mpv-prop-change",
                                PlayerEvent::PropChange(PlayerProprChange::from_name_value(
                                    name.to_string(),
                                    change,
                                )),
                            )
                            .to_value(),

                            Ok(Event::EndFile(reason)) => PlayerResponse(
                                "mpv-event-ended",
                                PlayerEvent::End(PlayerEnded::from_end_reason(reason)),
                            )
                            .to_value(),
                            Ok(Event::Shutdown) => {
                                break 'main;
                            }
                            _ => None,
                        };
                        if resp_event.is_some() {
                            tx1.send(RPCResponse::response_message(resp_event)).ok();
                        }
                    } // event processing loop
                } // main loop
            }) // crossbeam scope
        }); // builder thread
        Ok(())
    }
}
