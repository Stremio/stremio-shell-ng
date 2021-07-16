use native_windows_gui::{self as nwg, PartialUi};
use std::cell::RefCell;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Default)]
pub struct Player {
    pub channel: RefCell<Option<(mpsc::Sender<String>, Arc<Mutex<mpsc::Receiver<String>>>)>>,
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
            let mut mpv = mpv_builder.build().unwrap();
            'main: loop {
                // wait up to 0.0 seconds for an event.
                while let Some(event) = mpv.wait_event(0.0) {
                    // even if you don't do anything with the events, it is still necessary to empty
                    // the event loop
                    // TODO: Parse and format the Event in proper JSON format
                    tx1.send(format!("{:?}", event)).ok();
                    println!("RECEIVED EVENT : {:?}", event);
                    match event {
                        mpv::Event::Shutdown | mpv::Event::EndFile(_) => {
                            break 'main;
                        }
                        _ => {}
                    };
                }
                if let Ok(msg) = rx.try_recv() {
                    println!("PLAYER RECEIVED MESSAGE: {}", msg);
                    // let video_path = "http://distribution.bbb3d.renderfarming.net/video/mp4/bbb_sunflower_1080p_30fps_normal.mp4";
                    // mpv.command(&["loadfile", video_path]).ok();
                    // mpv.command(&["stop"]).ok();
                    // mpv.set_property("paused", true).ok();
                }
            }
        });

        Ok(())
    }
}
