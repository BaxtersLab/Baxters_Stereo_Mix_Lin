// bsm-ui — egui recorder UI + telemetry client
pub mod theme;
pub mod window;
pub mod status;
pub mod telemetry_client;

pub use theme::apply_bsm_theme;
pub use theme::colors;

/// The launcher's desktop file ID, given to the window as its Wayland app id.
///
/// GNOME matches a window to its launcher, and so picks its dock icon, by the
/// app id. The window set none, eframe named it after the title, nothing
/// matched, and an installed Stereo Mix showed a generic gear in the dock
/// (RC-2026-09-28-r2 VM check; operator ruling 2026-09-29).
pub const APP_ID: &str = "baxters-stereo-mix";

#[cfg(test)]
mod window_identity_tests {
    use super::APP_ID;
    use std::path::Path;

    #[test]
    fn the_app_id_is_the_shipped_launcher() {
        let launcher = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packaging/usr/share/applications")
            .join(format!("{APP_ID}.desktop"));
        assert!(launcher.is_file(), "no shipped launcher named {APP_ID}.desktop");
    }

    #[test]
    fn the_window_is_given_the_app_id() {
        let main = include_str!("main.rs");
        assert!(
            main.contains(".with_app_id(bsm_ui::APP_ID)"),
            "main.rs must give the window APP_ID, or the dock cannot match it to its launcher"
        );
    }
}
