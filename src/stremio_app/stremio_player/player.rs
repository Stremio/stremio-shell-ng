use crate::stremio_app::ipc;
use crate::stremio_app::RPCResponse;
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
                .set_option("title", "Stremio")
                .expect("failed setting title");
            mpv_builder
                .set_option("terminal", "yes")
                .expect("failed setting terminal");
            mpv_builder
                .set_option("msg-level", "all=no,cplayer=debug")
                .expect("failed setting msg-level");
            mpv_builder
                .set_option("quiet", "yes")
                .expect("failed setting msg-level");
            let mut mpv = mpv_builder.build().expect("Cannot build MPV");

            'main: loop {
                // wait up to X seconds for an event.
                while let Some(event) = mpv.wait_event(0.0) {
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
                for msg in rx.try_iter() {
                    match serde_json::from_str::<InMsg>(msg.as_str()) {
                        Ok(InMsg(
                            InMsgFn::MpvObserveProp,
                            InMsgArgs::ObProp(PropKey::Bool(prop)),
                        )) => mpv.observe_property::<bool>(prop.to_string().as_str(), 0),
                        Ok(InMsg(
                            InMsgFn::MpvObserveProp,
                            InMsgArgs::ObProp(PropKey::Int(prop)),
                        )) => mpv.observe_property::<i64>(prop.to_string().as_str(), 0),
                        Ok(InMsg(
                            InMsgFn::MpvObserveProp,
                            InMsgArgs::ObProp(PropKey::Fp(prop)),
                        )) => mpv.observe_property::<f64>(prop.to_string().as_str(), 0),
                        Ok(InMsg(
                            InMsgFn::MpvObserveProp,
                            InMsgArgs::ObProp(PropKey::Str(prop)),
                        )) => mpv.observe_property::<&str>(prop.to_string().as_str(), 0),
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
                            mpv.command(&cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>())
                        }
                        _ => {
                            eprintln!("MPV unsupported message {}", msg);
                            Ok(())
                        }
                    }
                    .ok();
                } // incoming message drain loop
            } // main loop
        }); // builder thread
        Ok(())
    }
}
