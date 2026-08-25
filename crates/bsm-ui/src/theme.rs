pub fn apply_bsm_theme(ctx: &eframe::egui::Context) {
    use eframe::egui::{Visuals, Color32, Stroke};
    let mut visuals = Visuals::dark();
    // background charcoal
    visuals.extreme_bg_color = Color32::from_rgb(40, 40, 40);
    visuals.panel_fill = Color32::from_rgb(40, 40, 40);
    visuals.faint_bg_color = Color32::from_rgb(40, 40, 40);
    // default text color -> white
    visuals.override_text_color = Some(Color32::WHITE);
    // selection and accent colors
    visuals.selection.bg_fill = Color32::from_rgb(80, 80, 80);
    visuals.hyperlink_color = Color32::from_rgb(180, 200, 255);
    // set thin black outlines for widgets
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::BLACK);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, Color32::BLACK);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::BLACK);
    visuals.widgets.inactive.rounding = eframe::egui::Rounding::same(4.0);
    visuals.widgets.active.rounding = eframe::egui::Rounding::same(4.0);
    visuals.window_rounding = eframe::egui::Rounding::same(6.0);
    ctx.set_visuals(visuals);
}

pub mod colors {
    pub const BACKGROUND: [f32; 4] = [0.156, 0.156, 0.156, 1.0];
}
