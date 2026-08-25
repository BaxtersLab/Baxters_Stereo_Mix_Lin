use bsm_ui::window::BsmWindow;
use bsm_ui::theme;

/// Single-instance TCP loopback lock.
const INSTANCE_LOCK_PORT: u16 = 51840;

fn acquire_instance_lock() -> Option<std::net::TcpListener> {
    use std::net::{TcpListener, Ipv4Addr, SocketAddrV4};
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, INSTANCE_LOCK_PORT)).ok()
}

fn main() {
    let _lock = match acquire_instance_lock() {
        Some(l) => l,
        None => {
            eprintln!("Baxter's Stereo Mix is already running.");
            return;
        }
    };

    // Decode splash/icon PNG at startup.
    const SPLASH_PNG: &[u8] = include_bytes!("../../../assets/sm512x512.png");
    let icon = match image::load_from_memory(SPLASH_PNG) {
        Ok(img) => {
            let icon_img = image::imageops::resize(
                &img.to_rgba8(), 64, 64,
                image::imageops::FilterType::Lanczos3,
            );
            egui::IconData { rgba: icon_img.into_raw(), width: 64, height: 64 }
        }
        Err(_) => egui::IconData { rgba: vec![0u8; 32 * 32 * 4], width: 32, height: 32 },
    };

    let mut options = eframe::NativeOptions::default();
    options.viewport = egui::ViewportBuilder::default()
        .with_inner_size(egui::vec2(600.0, 500.0))
        .with_resizable(true)
        .with_icon(std::sync::Arc::new(icon));

    eframe::run_native("Baxter's Stereo Mix", options, Box::new(|cc| {
        theme::apply_bsm_theme(&cc.egui_ctx);
        Ok(Box::new(BsmWindow::new()))
    })).unwrap();
}
