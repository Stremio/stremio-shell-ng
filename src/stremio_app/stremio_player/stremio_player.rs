use native_windows_gui::{self as nwg, PartialUi};
use std::cell::RefCell;

#[derive(Default)]
pub struct Player {
    mpv: RefCell<Option<mpv::MpvHandler>>,
}

impl Player {
    pub fn command(&self, args: &[&str]) {
        let mut mpv = self.mpv.borrow_mut();
        let mpv = mpv.as_mut().expect("Failed to create MPV");
        if let Err(e) = mpv.command(args) {
            eprintln!("Failed to execute command {:?} - {:?}", args, e);
        }
    }
    pub fn set_prop<T: mpv::MpvFormat>(&self, prop: &str, val: T) {
        let mut mpv = self.mpv.borrow_mut();
        let mpv = mpv.as_mut().expect("Failed to create MPV");
        if let Err(e) = mpv.set_property(prop, val) {
            eprintln!("Failed to set property {} - {:?}", prop, e);
        }
    }
}

impl PartialUi for Player {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        let mut mpv_builder =
            mpv::MpvHandlerBuilder::new().expect("Error while creating MPV builder");
        mpv_builder
            .set_option(
                "wid",
                parent
                    .expect("No parent window")
                    .into()
                    .hwnd()
                    .expect("Cannot obtain window handle") as i64,
            )
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
        data.mpv = RefCell::new(mpv_builder.build().ok());
        Ok(())
    }
}
