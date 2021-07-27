use native_windows_derive::NwgPartial;
use native_windows_gui as nwg;
use std::cmp;

#[derive(Default, NwgPartial)]
pub struct SplashImage {
    #[nwg_resource]
    embed: nwg::EmbedResource,
    #[nwg_resource(size: Some((300,300)), source_embed: Some(&data.embed), source_embed_str: Some("SPLASHIMAGE"))]
    splash_image: nwg::Bitmap,
    #[nwg_control(background_color: Some(Self::BG_COLOR))]
    splash_frame: nwg::ImageFrame,
    #[nwg_control(parent: splash_frame, background_color: Some(Self::BG_COLOR), bitmap: Some(&data.splash_image))]
    splash: nwg::ImageFrame,
}

impl SplashImage {
    const BG_COLOR: [u8; 3] = [27, 17, 38];
    pub fn resize(&self, size:(u32, u32)) {
        let (w, h) = size;
        let s = cmp::min(w, h);
        self.splash_frame.set_size(w, h);
        self.splash.set_size(s, s);
        self.splash.set_position(w as i32 / 2 - s as i32 / 2, 0);
    }
    pub fn visible(&self) -> bool {
        self.splash_frame.visible()
    }
    pub fn hide(&self) {
        self.splash_frame.set_visible(false);
    }
}