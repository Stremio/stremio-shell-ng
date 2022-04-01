use crate::stremio_app::ipc;
use crate::stremio_app::RPCResponse;
use flume::{Receiver, Sender};
use libmpv::{Mpv, events::Event, Format, SetData};
use native_windows_gui::{self as nwg, PartialUi};
use winapi::shared::windef::HWND;
use std::{thread::{self, JoinHandle}, sync::Arc};

use crate::stremio_app::stremio_player::{
    InMsg, InMsgArgs, InMsgFn, PlayerEnded, PlayerEvent, PlayerProprChange, PlayerResponse,
    PropKey, PropVal, CmdVal,
};

struct ObserveProperty {
    name: String,
    format: Format,
}

#[derive(Default)]
pub struct Player {
    pub channel: ipc::Channel,
}

impl PartialUi for Player {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        // @TODO replace with `&mut self`?
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        let (in_msg_sender, in_msg_receiver) = flume::unbounded();
        let (rpc_response_sender, rpc_response_receiver) = flume::unbounded();

        data.channel = ipc::Channel::new(Some((in_msg_sender, rpc_response_receiver)));

        let window_handle = parent
            .expect("no parent window")
            .into()
            .hwnd()
            .expect("cannot obtain window handle");
        // @TODO replace all `expect`s with proper error handling?

        let mpv = create_shareable_mpv(window_handle);
        let (observe_property_sender, observe_property_receiver) = flume::unbounded();

        let _event_thread = create_event_thread(Arc::clone(&mpv), observe_property_receiver, rpc_response_sender);
        let _message_thread = create_message_thread(mpv, observe_property_sender, in_msg_receiver);
        // @TODO implement a mechanism to stop threads on `Player` drop if needed
        
        Ok(())
    }
}

fn create_shareable_mpv(window_handle: HWND) -> Arc<Mpv> {
    let mpv = Mpv::with_initializer(|initializer| {
        initializer.set_property("wid", window_handle as i64).expect("failed setting wid");
        // initializer.set_property("vo", "gpu").expect("unable to set vo");
        // win, opengl: works but least performancy, 10-15% CPU
        // winvk, vulkan: works as good as d3d11
        // d3d11, d1d11: works great
        // dxinterop, auto: works, slightly more cpu use than d3d11
        // default (auto) seems to be d3d11 (vo/gpu/d3d11)
        initializer.set_property("gpu-context", "angle").expect("failed setting gpu-contex");
        initializer.set_property("gpu-api", "auto").expect("failed setting gpu-api");
        initializer.set_property("title", "Stremio").expect("failed setting title");
        initializer.set_property("terminal", "yes").expect("failed setting terminal");
        initializer.set_property("msg-level", "all=no,cplayer=debug").expect("failed setting msg-level");
        initializer.set_property("quiet", "yes").expect("failed setting quiet");
        initializer.set_property("hwdec", "auto").expect("failed setting hwdec");
        // FIXME: very often the audio track isn't selected when using "aid" = "auto"
        initializer.set_property("aid", 1).expect("failed setting aid");
        Ok(())
    }).expect("cannot build MPV");

    Arc::new(mpv)
}

fn create_event_thread(
    mpv: Arc<Mpv>,
    observe_property_receiver: Receiver<ObserveProperty>, 
    rpc_response_sender: Sender<String>
) -> JoinHandle<()> {
    thread::spawn(move || { 
        let mut event_context = mpv.create_event_context();
        event_context.disable_deprecated_events().expect("failed to disable deprecated MPV events");

        loop {
            for ObserveProperty { name, format } in observe_property_receiver.drain() {
                event_context.observe_property(&name, format, 0).expect("failed to observer MPV property");
            }

            // -1.0 means to block and wait for an event.
            let event = match event_context.wait_event(-1.) {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    eprintln!("Event errored: {error:?}");
                    continue; 
                }
                // dummy event received (may be created on a wake up call or on timeout)
                None => continue,
            };

            // even if you don't do anything with the events, it is still necessary to empty the event loop
            let resp_event = match event {
                Event::PropertyChange {
                    name,
                    change,
                    ..
                } => PlayerResponse(
                    "mpv-prop-change",
                    PlayerEvent::PropChange(PlayerProprChange::from_name_value(
                        name.to_string(),
                        change,
                    )),
                )
                .to_value(),
                Event::EndFile(reason) => PlayerResponse(
                    "mpv-event-ended",
                    PlayerEvent::End(PlayerEnded::from_end_reason(reason)),
                )
                .to_value(),
                Event::Shutdown => {
                    break;
                }
                _ => None,
            };
            if resp_event.is_some() {
                rpc_response_sender.send(RPCResponse::response_message(resp_event)).ok();
            }
        }
    })
}

fn create_message_thread(
    mpv: Arc<Mpv>, 
    observe_property_sender: Sender<ObserveProperty>, 
    in_msg_receiver: Receiver<String>
) -> JoinHandle<()> {
    thread::spawn(move || {
        // -- Helpers --

        let observe_property = |name: String, format: Format| {
            observe_property_sender.send(ObserveProperty { name, format }).expect("cannot send ObserveProperty");
            mpv.wake_up();
        };

        let send_command = |cmd: CmdVal| {
            let (name, arg) = match cmd {
                CmdVal::Double(name, arg) => (name, format!(r#""{arg}""#)),
                CmdVal::Single((name,)) => (name, String::new())
            };
            mpv.command(&name.to_string(), &[&arg]).expect("failed to execute MPV command");
        };

        fn set_property(name: impl ToString, value: impl SetData, mpv: &Mpv) {
            if let Err(error) = mpv.set_property(&name.to_string(), value) {
                eprintln!("cannot set MPV property: '{error:#}'")
            };
        }

        // -- InMsg handler loop --

        for msg in in_msg_receiver.iter() {
            let in_msg: InMsg = match serde_json::from_str(&msg) {
                Ok(in_msg) => in_msg,
                Err(error) => {
                    eprintln!("cannot parse InMsg: {error:#}");
                    continue;
                }
            };

            match in_msg {
                InMsg(
                    InMsgFn::MpvObserveProp,
                    InMsgArgs::ObProp(PropKey::Bool(prop)),
                ) => {
                    observe_property(prop.to_string(), Format::Flag);
                },
                InMsg(
                    InMsgFn::MpvObserveProp,
                    InMsgArgs::ObProp(PropKey::Int(prop)),
                ) => {
                    observe_property(prop.to_string(), Format::Int64);
                },
                InMsg(
                    InMsgFn::MpvObserveProp,
                    InMsgArgs::ObProp(PropKey::Fp(prop)),
                ) => {
                    observe_property(prop.to_string(), Format::Double);
                },
                InMsg(
                    InMsgFn::MpvObserveProp,
                    InMsgArgs::ObProp(PropKey::Str(prop)),
                ) => {
                    observe_property(prop.to_string(), Format::String);
                },
                InMsg(
                    InMsgFn::MpvSetProp,
                    InMsgArgs::StProp(prop, PropVal::Bool(value)),
                ) => {
                    set_property(prop, value, &mpv);
                }
                InMsg(
                    InMsgFn::MpvSetProp,
                    InMsgArgs::StProp(prop, PropVal::Num(value)),
                ) => {
                    set_property(prop, value, &mpv);
                }
                InMsg(
                    InMsgFn::MpvSetProp,
                    InMsgArgs::StProp(prop, PropVal::Str(value)),
                ) => {
                    set_property(prop, value, &mpv);
                }
                InMsg(InMsgFn::MpvCommand, InMsgArgs::Cmd(cmd)) => {
                    send_command(cmd);
                }
                msg => {
                    eprintln!("MPV unsupported message: '{msg:?}'");
                }
            }
        }
    })
}


trait MpvExt {
    fn wake_up(&self);
} 

impl MpvExt for Mpv {
    // @TODO create a PR to the `libmpv` crate and then remove `libmpv-sys` from Cargo.toml?
    fn wake_up(&self) {
        unsafe { libmpv_sys::mpv_wakeup(self.ctx.as_ptr()) }
    }
}
