use native_windows_derive::NwgPartial;
use native_windows_gui as nwg;

#[derive(Default, NwgPartial)]
pub struct SystemTray {
    #[nwg_resource]
    pub embed: nwg::EmbedResource,
    #[nwg_resource(source_embed: Some(&data.embed), source_embed_str: Some("MAINICON"))]
    pub tray_icon: nwg::Icon,
    #[nwg_control(icon: Some(&data.tray_icon), tip: Some("Stremio"))]
    #[nwg_events(MousePressLeftUp: [Self::show_menu], OnContextMenu: [Self::show_menu])]
    pub tray: nwg::TrayNotification,
    #[nwg_control(popup: true)]
    pub tray_menu: nwg::Menu,
    #[nwg_control(parent: tray_menu, text: "&Show window")]
    pub tray_show_hide: nwg::MenuItem,
    #[nwg_control(parent: tray_menu, text: "Always on &top")]
    pub tray_topmost: nwg::MenuItem,
    #[nwg_control(parent: tray_menu, text: "&Quit")]
    pub tray_exit: nwg::MenuItem,
}

impl SystemTray {
    fn show_menu(&self) {
        let (x, y) = nwg::GlobalCursor::position();
        self.tray_menu.popup(x, y);
    }
}
