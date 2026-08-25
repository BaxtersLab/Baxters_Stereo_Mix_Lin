use eframe::egui::{self, CentralPanel, Context, SelectableLabel, ScrollArea};
use bsm_core::PcmFormat;
use bsm_audio::wasapi::{WasapiBackend, NegotiationInfo};
use bsm_audio::pipeline::{CapturePipeline, DeviceConfig};
use tokio::runtime::{Runtime, Builder};
use tracing;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::task::JoinHandle;
use tokio::sync::mpsc;
use bsm_core::DeviceEntry;
use std::path::PathBuf;
// tempfile no longer required here
use bsm_encode::muxer::{create_muxer, ContainerFormat};
use bsm_encode::encoder::AudioPacket as EncAudioPacket;
use rfd::FileDialog;
use dirs_next;
use serde::{Serialize, Deserialize};
use std::fs;
use std::io::Write;

#[derive(Serialize, Deserialize)]
struct UiConfig {
    output_dir: PathBuf,
    bitrate_kbps: Option<u32>,
    container: Option<String>,
}

impl UiConfig {
    fn path() -> Option<PathBuf> {
        dirs_next::config_dir().map(|d| d.join("bsm-ui").join("config.json"))
    }

    fn load() -> Option<Self> {
        let p = Self::path()?;
        if !p.exists() { return None; }
        match fs::read_to_string(&p) {
            Ok(s) => serde_json::from_str(&s).ok(),
            Err(_) => None,
        }
    }

    fn save(&self) -> Result<(), String> {
        let p = Self::path().ok_or_else(|| "no config dir".to_string())?;
        if let Some(parent) = p.parent() { fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
        let s = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let mut f = fs::File::create(&p).map_err(|e| e.to_string())?;
        f.write_all(s.as_bytes()).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_window_defaults() {
        let w = BsmWindow::new();
        assert_eq!(w.sample_rates.get(0).copied().unwrap_or(0), 48000);
        assert!(w.peak_vals.lock().is_ok());
    }
}

pub struct BsmWindow {
    pipeline: Arc<Mutex<CapturePipeline<WasapiBackend>>>,
    devices: Vec<DeviceEntry>,
    selected_device: usize,
    sample_rates: Vec<u32>,
    selected_rate_idx: usize,
    rt: Runtime,
    logs: Vec<String>,
    last_diag: Option<NegotiationInfo>,
    peak_vals: Arc<Mutex<(f32, f32)>>,
    volume: f32,
    smoothed_peaks: (f32, f32),
    meter_decay: f32,
    peak_hold: (f32, f32),
    mono: bool,
    recording: bool,
    recording_paused: bool,
    record_thread: Option<JoinHandle<()>>,
    record_path: Option<PathBuf>,
    record_stop: Option<Arc<AtomicBool>>,
    monitor_thread: Option<JoinHandle<()>>,
    monitor_stop: Option<Arc<AtomicBool>>,
    record_tx: Option<mpsc::Sender<EncAudioPacket>>,
    output_dir: PathBuf,
    bitrate_kbps: u32,
    container_format: ContainerFormat,
    telemetry_rx: Option<std::sync::mpsc::Receiver<String>>,
    splash_start: std::time::Instant,
    in_splash: bool,
    bsm_debug: bool,
}

impl BsmWindow {
    pub fn new() -> Self {
        let backend = WasapiBackend::new();
        let fmt = PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 };
        let pipeline = CapturePipeline::new(backend, fmt.clone());
        let pipeline = Arc::new(Mutex::new(pipeline));
        let rt = Builder::new_multi_thread().enable_time().build().expect("failed to create tokio runtime");
        let cfg = UiConfig::load();
        let output_dir = cfg.as_ref().map(|c| c.output_dir.clone()).unwrap_or_else(|| {
            if let Some(d) = dirs_next::audio_dir() {
                d.join("BSM_output")
            } else if let Some(h) = dirs_next::home_dir() {
                h.join("Music").join("BSM_output")
            } else {
                PathBuf::from("BSM_output")
            }
        });
        let bitrate_kbps = cfg.as_ref().and_then(|c| c.bitrate_kbps).unwrap_or(128);
        let container_format = cfg.as_ref().and_then(|c| c.container.as_ref().map(|s| s.as_str())).and_then(|s| match s {
            "mp3" => Some(ContainerFormat::Mp3),
            "flac" => Some(ContainerFormat::Flac),
            _ => Some(ContainerFormat::Wav),
        }).unwrap_or(ContainerFormat::Wav);
        // If there was no config on disk, persist our chosen defaults so output
        // files land in the expected `BSM_output` folder by default.
        if cfg.is_none() {
            let save_cfg = UiConfig { output_dir: output_dir.clone(), bitrate_kbps: Some(bitrate_kbps), container: Some(match container_format { ContainerFormat::Wav => "wav", ContainerFormat::Mp3 => "mp3", ContainerFormat::Flac => "flac" }.into()) };
            if let Err(e) = save_cfg.save() { eprintln!("failed to save default config: {}", e); }
        }

        // Ensure the output directory exists (create it if missing).
        if let Err(e) = std::fs::create_dir_all(&output_dir) {
            eprintln!("failed to create output dir {:?}: {}", output_dir, e);
        }
        Self {
            pipeline,
            devices: Vec::new(),
            selected_device: 0,
            sample_rates: vec![48000, 44100, 96000],
            selected_rate_idx: 0,
            rt,
            logs: Vec::new(),
            last_diag: None,
            peak_vals: Arc::new(Mutex::new((0.0, 0.0))),
            volume: 1.0,
            smoothed_peaks: (0.0, 0.0),
            meter_decay: 0.92,
            peak_hold: (0.0, 0.0),
            mono: false,
            recording: false,
            recording_paused: false,
            record_thread: None,
            record_path: None,
            record_stop: None,
            record_tx: None,
            output_dir,
            bitrate_kbps,
            container_format,
            telemetry_rx: None,
            monitor_thread: None,
            monitor_stop: None,
            splash_start: std::time::Instant::now(),
            in_splash: true,
            bsm_debug: std::env::var("BSM_DEBUG").unwrap_or_default() == "1",
        }
    }

    fn start_recording(&mut self) {
        // stop any running monitor thread so capture isn't raced
        if let Some(flag) = self.monitor_stop.take() {
            flag.store(true, Ordering::Relaxed);
        }
        if let Some(h) = self.monitor_thread.take() {
            let _ = self.rt.block_on(async { let _ = h.await; });
        }

        let pipeline = Arc::clone(&self.pipeline);
        let fmt = { let p = self.pipeline.lock().unwrap(); p.requested_format().clone() };
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let fname = format!("recording-{}.{}", ts, self.container_format.extension());
        let path = self.output_dir.join(fname);
        if let Err(e) = std::fs::create_dir_all(&self.output_dir) { self.logs.push(format!("failed to create output dir: {:?}", e)); }
        let mut mux = create_muxer(self.container_format);
        let codec_name = match self.container_format {
            ContainerFormat::Wav  => "pcm_s16le",
            ContainerFormat::Mp3  => "mp3",
            ContainerFormat::Flac => "flac",
        };
        if let Err(e) = mux.open(&path, &fmt, codec_name, self.bitrate_kbps) {
            self.logs.push(format!("record: open muxer failed: {:?}", e));
            return;
        }
        self.logs.push(format!("recording started: {}", path.to_string_lossy()));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_thread = Arc::clone(&stop_flag);
        let (tx, mut rx) = mpsc::channel::<EncAudioPacket>(128);
        self.record_tx = Some(tx.clone());
        self.record_stop = Some(Arc::clone(&stop_flag));

        let tx_cap = tx.clone();
        let cap_stop = Arc::clone(&stop_flag_thread);
        let pipeline_cap = Arc::clone(&pipeline);
        let peak_vals_clone = Arc::clone(&self.peak_vals);
        let _cap_handle = self.rt.handle().spawn_blocking(move || {
            let rt = Builder::new_current_thread().enable_time().build().expect("rt");
            rt.block_on(async move {
                loop {
                    if cap_stop.load(Ordering::Relaxed) { break; }
                    let next = { let mut p = pipeline_cap.lock().unwrap(); p.next_frame().await };
                    match next {
                        Ok(Some(frame)) => {
                            // compute per-channel peak from 16-bit interleaved samples
                            if frame.format.bit_depth == 16 {
                                let bytes = &frame.data;
                                let channels = frame.format.channels as usize;
                                let mut sample_idx = 0usize;
                                let mut max0: i32 = 0;
                                let mut max1: i32 = 0;
                                let mut i = 0usize;
                                while i + 1 < bytes.len() {
                                    let sample = i16::from_le_bytes([bytes[i], bytes[i+1]]);
                                    let absv = sample.abs() as i32;
                                    if channels == 1 {
                                        if absv > max0 { max0 = absv; }
                                    } else {
                                        if sample_idx % channels == 0 {
                                            if absv > max0 { max0 = absv; }
                                        } else {
                                            if absv > max1 { max1 = absv; }
                                        }
                                    }
                                    sample_idx += 1;
                                    i += 2;
                                }
                                let peak_l = (max0 as f32) / (i16::MAX as f32);
                                let peak_r = if frame.format.channels > 1 { (max1 as f32) / (i16::MAX as f32) } else { peak_l };
                                if let Ok(mut pk) = peak_vals_clone.lock() { *pk = (peak_l, peak_r); }
                            }

                            let pkt = EncAudioPacket { data: frame.data.clone(), pts: frame.timestamp_us as i64, duration: frame.frame_count as i64, is_key: true };
                            if tx_cap.send(pkt).await.is_err() { break; }
                        }
                        Ok(None) => { tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
                        Err(_) => break,
                    }
                }
            });
        });

        let mux_stop = Arc::clone(&stop_flag_thread);
        let mux_handle = self.rt.handle().spawn_blocking(move || {
            let rt = Builder::new_current_thread().enable_time().build().expect("rt");
            rt.block_on(async move {
                while let Some(pkt) = rx.recv().await {
                    if mux_stop.load(Ordering::Relaxed) { break; }
                    let _ = mux.write_packet(&pkt);
                }
                let _ = mux.finalize();
            });
        });
        self.recording = true;
        self.record_thread = Some(mux_handle);
        self.record_path = Some(path.clone());
    }

    fn stop_recording(&mut self) {
        if let Some(flag) = self.record_stop.take() {
            flag.store(true, Ordering::Relaxed);
        }
        let _ = self.record_tx.take();
        if let Some(h) = self.record_thread.take() {
            use std::time::{Instant, Duration};
            let start = Instant::now();
            let timeout = Duration::from_millis(200);
            loop {
                if h.is_finished() { let _ = self.rt.block_on(async { let _ = h.await; }); break; }
                if start.elapsed() >= timeout { h.abort(); let _ = self.rt.block_on(async { let _ = h.await; }); break; }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if let Some(p) = self.record_path.take() { self.logs.push(format!("recording saved: {}", p.to_string_lossy())); }
        self.recording = false;
        self.recording_paused = false;

        // allow monitor thread to be restarted on next update()
        self.monitor_thread = None;
        self.monitor_stop = None;
    }
}
impl eframe::App for BsmWindow {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // ── Splash screen (1.5 s) ────────────────────────────────────────
        const SPLASH_MS: u128 = 1500;
        if self.in_splash {
            if self.splash_start.elapsed().as_millis() < SPLASH_MS {
                CentralPanel::default().show(ctx, |ui| {
                    let rect = ui.available_rect_before_wrap();
                    ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new("Baxter's Stereo Mix")
                                .size(32.0)
                                .color(egui::Color32::WHITE),
                        );
                    });
                });
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return;
            }
            self.in_splash = false;
        }

        if self.bsm_debug {
            tracing::debug!("bsm update tick");
        }

        let panel_frame = eframe::egui::Frame::none()
            .fill(eframe::egui::Color32::from_rgb(40,40,40))
            .stroke(eframe::egui::Stroke::new(1.0_f32, eframe::egui::Color32::BLACK))
            .rounding(egui::Rounding::same(6.0));

        let mut style = (*ctx.style()).clone();
        style.visuals.override_text_color = Some(egui::Color32::WHITE);
        ctx.set_style(style);

        CentralPanel::default().frame(panel_frame).show(ctx, |ui| {
            ui.set_min_size(egui::vec2(600.0, 420.0));
            ui.heading("Baxter's Stereo Mix — Audio Settings");
            ui.add_space(6.0);

            // Top framed section (devices + output folder + record controls)
            let top_section = eframe::egui::Frame::none()
                .fill(egui::Color32::from_rgb(40,40,40))
                .rounding(egui::Rounding::same(6.0))
                .inner_margin(egui::Margin::same(8.0));

            top_section.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        if ui.add(egui::Button::new(egui::RichText::new("Refresh Devices").color(egui::Color32::BLACK))).clicked() {
                            let pipeline = Arc::clone(&self.pipeline);
                            let rt2 = Builder::new_current_thread().enable_time().build().expect("rt");
                            if let Some(devs) = rt2.block_on(async move { let p = pipeline.lock().unwrap(); p.enumerate().await.ok() }) {
                                let len = devs.len();
                                self.devices = devs;
                                self.logs.push(format!("Refreshed {} devices", len));
                            }
                        }

                        ui.label("Devices:");
                        ScrollArea::vertical().max_height(180.0).id_salt("devices_scroll").show(ui, |ui| {
                            for (i, d) in self.devices.iter().enumerate() {
                                let prev_sel = self.selected_device;
                                let label = egui::RichText::new(&d.name).color(egui::Color32::WHITE);
                                let resp = ui.selectable_value(&mut self.selected_device, i, label);
                                if resp.clicked() || (prev_sel != self.selected_device && self.selected_device == i) {
                                    self.logs.push(format!("Selected device {}", d.name));
                                    // Use the device's real index (the list is not 1:1 with cpal
                                    // indices — "System Audio (Loopback)" is prepended).
                                    let dev_index = d.index;
                                    let fmt = { let p = self.pipeline.lock().unwrap(); p.requested_format().clone() };
                                    // Sample-rate enumeration + negotiation diagnostics are
                                    // cpal-input-device specific; the loopback endpoint uses the
                                    // render mix format automatically, so skip them for it.
                                    if !d.is_loopback {
                                        if let Ok(r) = WasapiBackend::enumerate_supported_sample_rates(dev_index, fmt.channels) {
                                            self.sample_rates = r; self.selected_rate_idx = 0;
                                            self.logs.push(format!("Found {} sample-rates", self.sample_rates.len()));
                                        }
                                        if let Ok(diag) = WasapiBackend::diagnose_negotiation(dev_index, fmt.clone()) {
                                            self.last_diag = Some(diag);
                                        }
                                    } else {
                                        self.last_diag = None;
                                    }

                                        // Attempt to open and start the selected device on the pipeline
                                        let pipeline_clone = Arc::clone(&self.pipeline);
                                        let fmt_clone = fmt.clone();
                                        let idx_u = dev_index;
                                        let rt2 = Builder::new_current_thread().enable_time().build().expect("rt");
                                        let open_res = rt2.block_on(async move {
                                            let mut p = pipeline_clone.lock().unwrap();
                                            p.open_with_config(idx_u, DeviceConfig::with_format(fmt_clone)).await
                                        });
                                        match open_res {
                                            Ok(()) => {
                                                // start streaming so monitor/recording can read frames
                                                let pipeline_clone2 = Arc::clone(&self.pipeline);
                                                let rt3 = Builder::new_current_thread().enable_time().build().expect("rt");
                                                let start_res = rt3.block_on(async move { let mut p = pipeline_clone2.lock().unwrap(); p.start().await });
                                                if start_res.is_ok() { self.logs.push(format!("Device {} opened and started", d.name)); }
                                                else { self.logs.push(format!("Device opened but failed to start: {:?}", start_res.err())); }
                                            }
                                            Err(e) => { self.logs.push(format!("Failed to open device: {:?}", e)); }
                                        }
                                }
                            }
                        });

                        // Container selector placed directly under Devices for quicker access
                        ui.add_space(6.0);
                        egui::ComboBox::from_label("Container")
                            .selected_text(match self.container_format {
                                ContainerFormat::Wav => egui::RichText::new("wav").color(egui::Color32::BLACK),
                                ContainerFormat::Mp3 => egui::RichText::new("mp3").color(egui::Color32::BLACK),
                                ContainerFormat::Flac => egui::RichText::new("flac").color(egui::Color32::BLACK),
                            })
                            .show_ui(ui, |ui| {
                                if ui.selectable_value(&mut (self.container_format), ContainerFormat::Wav, egui::RichText::new("wav").color(egui::Color32::BLACK)).clicked() {
                                    let cfg = UiConfig { output_dir: self.output_dir.clone(), bitrate_kbps: Some(self.bitrate_kbps), container: Some("wav".into()) };
                                    let _ = cfg.save();
                                }
                                if ui.selectable_value(&mut (self.container_format), ContainerFormat::Mp3, egui::RichText::new("mp3").color(egui::Color32::BLACK)).clicked() {
                                    let cfg = UiConfig { output_dir: self.output_dir.clone(), bitrate_kbps: Some(self.bitrate_kbps), container: Some("mp3".into()) };
                                    let _ = cfg.save();
                                }
                                if ui.selectable_value(&mut (self.container_format), ContainerFormat::Flac, egui::RichText::new("flac").color(egui::Color32::BLACK)).clicked() {
                                    let cfg = UiConfig { output_dir: self.output_dir.clone(), bitrate_kbps: Some(self.bitrate_kbps), container: Some("flac".into()) };
                                    let _ = cfg.save();
                                }
                        });

                        ui.add_space(6.0);
                        ui.label("Sample Rates:");
                        for (i, r) in self.sample_rates.iter().enumerate() {
                            if ui.add(SelectableLabel::new(self.selected_rate_idx == i, format!("{} Hz", r))).clicked() {
                                self.selected_rate_idx = i;
                                self.logs.push(format!("Selected sample rate {} Hz", r));
                                let mut fmt = { let p = self.pipeline.lock().unwrap(); p.requested_format().clone() };
                                fmt.sample_rate = *r;
                                {
                                    let mut p = self.pipeline.lock().unwrap();
                                    p.set_requested_format(fmt.clone());
                                }
                                if let Ok(diag) = WasapiBackend::diagnose_negotiation(self.devices.get(self.selected_device).map(|d| d.index).unwrap_or(u32::MAX), fmt.clone()) {
                                    self.last_diag = Some(diag);
                                }
                            }
                        }
                    });
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Output folder:");
                    ui.label(self.output_dir.to_string_lossy());
                    if ui.add(egui::Button::new(egui::RichText::new("Browse...").color(egui::Color32::BLACK))).clicked() {
                        if let Some(dir) = FileDialog::new().set_directory(&self.output_dir).pick_folder() {
                            self.output_dir = dir;
                            self.logs.push(format!("Output folder set: {}", self.output_dir.to_string_lossy()));
                            let cfg = UiConfig { output_dir: self.output_dir.clone(), bitrate_kbps: Some(self.bitrate_kbps), container: Some(match self.container_format { ContainerFormat::Wav => "wav", ContainerFormat::Mp3 => "mp3", ContainerFormat::Flac => "flac" }.into()) };
                            if let Err(e) = cfg.save() { self.logs.push(format!("failed to save config: {}", e)); }
                        }
                    }
                    ui.label(egui::RichText::new(self.output_dir.to_string_lossy()).monospace());
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    // Record / Pause / Stop controls
                    let rec_btn = egui::Button::new(egui::RichText::new("Record").color(egui::Color32::WHITE)).fill(egui::Color32::from_rgb(200,30,30));
                    let rec_resp = ui.add(rec_btn);
                    if rec_resp.clicked() {
                        if !self.recording { self.start_recording(); } else { self.logs.push("Already recording".into()); }
                    }

                    let pause_resp = ui.add(egui::Button::new(egui::RichText::new("Pause").color(egui::Color32::BLACK)));
                    if pause_resp.clicked() {
                        if self.recording {
                            self.recording_paused = !self.recording_paused;
                            self.logs.push(format!("Recording paused: {}", self.recording_paused));
                        }
                    }

                    let stop_resp = ui.add(egui::Button::new(egui::RichText::new("Stop").color(egui::Color32::BLACK)));
                    if stop_resp.clicked() {
                        if self.recording { self.stop_recording(); }
                    }

                    // If recording, draw a flashing outline to indicate active recording
                    if self.recording {
                        let painter = ui.painter();
                        let t = ctx.input(|i| i.time) as f32;
                        let alpha = ((t * 4.0).sin() * 0.5 + 0.5) * 0.8 + 0.2; // 0.2..1.0
                        let a = (alpha * 255.0).clamp(0.0, 255.0) as u8;
                        let glow = egui::Color32::from_rgba_unmultiplied(255, 100, 100, a);
                        painter.rect_stroke(rec_resp.rect, egui::Rounding::same(6.0), egui::Stroke::new(3.0_f32, glow));
                    }

                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("vol").color(egui::Color32::WHITE));
                    ui.add_sized(egui::vec2(140.0, 20.0), egui::Slider::new(&mut self.volume, 0.0..=1.0).fixed_decimals(2).show_value(false));
                    ui.add_space(6.0);
                    let value_size = egui::vec2(48.0, 20.0);
                    let (value_rect, _resp) = ui.allocate_exact_size(value_size, egui::Sense::hover());
                    let painter = ui.painter();
                    painter.text(value_rect.min + egui::vec2(2.0, 2.0), egui::Align2::LEFT_TOP, format!("{:.2}", self.volume), egui::FontId::proportional(14.0), egui::Color32::from_rgb(240,200,0));
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("ch").color(egui::Color32::WHITE));
                    if ui.selectable_value(&mut self.mono, false, "Stereo").clicked() {
                        self.logs.push("Set channels: stereo".into());
                        let mut fmt = { let p = self.pipeline.lock().unwrap(); p.requested_format().clone() };
                        fmt.channels = 2;
                        { let mut p = self.pipeline.lock().unwrap(); p.set_requested_format(fmt.clone()); }
                        if let Ok(diag) = WasapiBackend::diagnose_negotiation(self.devices.get(self.selected_device).map(|d| d.index).unwrap_or(u32::MAX), fmt) { self.last_diag = Some(diag); }
                    }
                    if ui.selectable_value(&mut self.mono, true, "Mono").clicked() {
                        self.logs.push("Set channels: mono".into());
                        let mut fmt = { let p = self.pipeline.lock().unwrap(); p.requested_format().clone() };
                        fmt.channels = 1;
                        { let mut p = self.pipeline.lock().unwrap(); p.set_requested_format(fmt.clone()); }
                        if let Ok(diag) = WasapiBackend::diagnose_negotiation(self.devices.get(self.selected_device).map(|d| d.index).unwrap_or(u32::MAX), fmt) { self.last_diag = Some(diag); }
                    }
                });
            });

            ui.separator();

            ui.vertical(|ui| {
                ui.label("Negotiation Diagnostics:");
                if let Some(diag) = &self.last_diag {
                    ui.label(format!("Device idx: {}", diag.device_index));
                    ui.label(format!("Requested: {} Hz, {} ch, {} bit", diag.requested_format.sample_rate, diag.requested_format.channels, diag.requested_format.bit_depth));
                    ui.label(format!("Chosen channels: {}", diag.chosen_channels));
                    ui.label(format!("Chosen SF: {:?}", diag.chosen_sample_format));
                    ui.label(format!("Negotiated SR: {} (range {}-{})", diag.negotiated_sample_rate, diag.chosen_min_sr, diag.chosen_max_sr));
                    ui.label(format!("Bit depth reported: {}", diag.bit_depth));
                    ui.label(format!("Score: {}", diag.score));
                    ui.label(format!("Candidate rates: {:?}", diag.candidate_rates));
                    let negotiated = { let p = self.pipeline.lock().unwrap(); p.negotiated_format() };
                    if let Some(n) = negotiated {
                        ui.label(format!("Pipeline negotiated: {} Hz, {} ch, {} bit", n.sample_rate, n.channels, n.bit_depth));
                    } else {
                        ui.label("Pipeline negotiated: <none>");
                    }
                } else {
                    ui.label("No diagnostics available; select a device and rate.");
                }

                // Container selector moved above (under Devices)

                if self.container_format == ContainerFormat::Mp3 {
                    egui::ComboBox::from_label("Bitrate (kbps)")
                        .selected_text(format!("{} kbps", self.bitrate_kbps))
                        .show_ui(ui, |ui| {
                            for &b in [128u32, 192, 256, 320].iter() {
                                if ui.selectable_value(&mut (self.bitrate_kbps), b, format!("{} kbps", b)).clicked() {
                                    let cfg = UiConfig { output_dir: self.output_dir.clone(), bitrate_kbps: Some(self.bitrate_kbps), container: Some(match self.container_format { ContainerFormat::Wav => "wav", ContainerFormat::Mp3 => "mp3", ContainerFormat::Flac => "flac" }.into()) };
                                    if let Err(e) = cfg.save() { self.logs.push(format!("failed to save config: {}", e)); }
                                }
                            }
                    });
                }

                ui.add_space(8.0);
                // read current peaks and apply simple attack/decay smoothing
                let (inst_l, inst_r) = if let Ok(pk) = self.peak_vals.lock() { *pk } else { (0.0, 0.0) };
                let prev_l = self.smoothed_peaks.0;
                let prev_r = self.smoothed_peaks.1;
                let new_l = if inst_l >= prev_l { inst_l } else { prev_l * self.meter_decay };
                let new_r = if inst_r >= prev_r { inst_r } else { prev_r * self.meter_decay };
                self.smoothed_peaks = (new_l, new_r);
                let combined = new_l.max(new_r);
                self.peak_hold.0 = self.peak_hold.0.max(combined);
                self.peak_hold.1 = self.peak_hold.1.max(combined);

                ui.label("Logs:");
                ScrollArea::vertical().max_height(100.0).auto_shrink(true).id_salt("logs_scroll").show(ui, |ui| {
                    for l in self.logs.iter().rev().take(50) {
                        ui.label(l);
                    }
                });
            });
        });

        // Always-visible bottom dB meter panel so it's visible even when main content scrolls.
        let bottom_frame = eframe::egui::Frame::none()
            .fill(egui::Color32::from_rgb(40,40,40))
            .stroke(egui::Stroke::new(1.0_f32, egui::Color32::BLACK))
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::same(8.0));

        egui::TopBottomPanel::bottom("db_meter_panel").frame(bottom_frame).show(ctx, |ui| {
            ui.set_min_height(36.0);
            ui.vertical(|ui| {
                ui.add_space(2.0);
                // Left and right meters now match height and are constrained to the
                // available width. Allocate a single rect for both meters so they
                // cannot extend beyond the right edge of the window.
                let meter_h = 18.0;
                let left_h = meter_h;
                let spacing = 4.0;
                let avail_w = ui.available_width();
                let total_h = left_h + spacing + meter_h;
                let (total_rect, _total_resp) = ui.allocate_exact_size(egui::vec2(avail_w, total_h), egui::Sense::hover());

                // top (left) meter rect
                let lrect = egui::Rect::from_min_size(total_rect.min, egui::vec2(total_rect.width(), left_h));
                // bottom (right) meter rect
                let rect = egui::Rect::from_min_max(
                    egui::pos2(total_rect.min.x, total_rect.min.y + left_h + spacing),
                    total_rect.max,
                );

                // painter after all ui allocations
                let painter = ui.painter();
                // draw left meter
                painter.rect_filled(lrect, egui::Rounding::same(0.0), egui::Color32::from_rgb(28,28,28));
                let left = (self.smoothed_peaks.0 * self.volume).clamp(0.0, 1.0);
                let ldb = if left <= 1e-6 { -60.0 } else { 20.0 * (left as f32).log10() };
                let ldb = ldb.max(-60.0);
                let lnorm = ((ldb + 60.0) / 60.0).clamp(0.0, 1.0);
                let lfill_w = lrect.width() * lnorm;
                let lfill_rect = egui::Rect::from_min_max(lrect.min, egui::pos2(lrect.min.x + lfill_w, lrect.max.y));
                painter.rect_filled(lfill_rect, egui::Rounding::same(0.0), egui::Color32::from_rgb(120,160,240));
                let lpeak_comb = (self.peak_hold.0 * self.volume).clamp(0.0, 1.0);
                let lpeak_db = if lpeak_comb <= 1e-6 { -60.0 } else { 20.0 * (lpeak_comb as f32).log10() };
                let lpeak_norm = ((lpeak_db.max(-60.0) + 60.0) / 60.0).clamp(0.0, 1.0);
                let lpeak_x = lrect.min.x + lrect.width() * lpeak_norm;
                painter.line_segment([egui::pos2(lpeak_x, lrect.top()), egui::pos2(lpeak_x, lrect.bottom())], egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(200,50,50)));
                let llabel = format!("L: {:.1} dB", ldb);
                painter.text(egui::pos2(lrect.min.x + 6.0, lrect.min.y + 1.0), egui::Align2::LEFT_TOP, llabel, egui::FontId::proportional(14.0), egui::Color32::WHITE);

                // draw right meter
                painter.rect_filled(rect, egui::Rounding::same(0.0), egui::Color32::from_rgb(24,24,24));
                let right = (self.smoothed_peaks.1 * self.volume).clamp(0.0, 1.0);
                let db = if right <= 1e-6 { -60.0 } else { 20.0 * (right as f32).log10() };
                let db = db.max(-60.0);
                let norm = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                let fill_w = rect.width() * norm;
                let fill_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + fill_w, rect.max.y));
                painter.rect_filled(fill_rect, egui::Rounding::same(0.0), egui::Color32::from_rgb(80,200,120));
                let peak_comb = (self.peak_hold.1 * self.volume).clamp(0.0, 1.0);
                let peak_db = if peak_comb <= 1e-6 { -60.0 } else { 20.0 * (peak_comb as f32).log10() };
                let peak_norm = ((peak_db.max(-60.0) + 60.0) / 60.0).clamp(0.0, 1.0);
                let peak_x = rect.min.x + rect.width() * peak_norm;
                painter.line_segment([egui::pos2(peak_x, rect.top()), egui::pos2(peak_x, rect.bottom())], egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(220,0,0)));
                let label = format!("R: {:.1} dB", db);
                painter.text(egui::pos2(rect.min.x + 6.0, rect.min.y + 1.0), egui::Align2::LEFT_TOP, label, egui::FontId::proportional(14.0), egui::Color32::WHITE);

                self.peak_hold.0 *= 0.995;
                self.peak_hold.1 *= 0.995;
            });
        });

        // Poll telemetry messages (if telemetry thread is running)
        // Ensure a lightweight monitor thread is running when not recording so meters show live input
        if !self.recording && self.monitor_thread.is_none() {
            let monitor_stop = Arc::new(AtomicBool::new(false));
            let monitor_stop_cl = Arc::clone(&monitor_stop);
            let pipeline_mon = Arc::clone(&self.pipeline);
            let peak_vals_mon = Arc::clone(&self.peak_vals);
            let handle = self.rt.handle().spawn_blocking(move || {
                let rt = Builder::new_current_thread().enable_time().build().expect("rt");
                rt.block_on(async move {
                    loop {
                        if monitor_stop_cl.load(Ordering::Relaxed) { break; }
                        let next = { let mut p = pipeline_mon.lock().unwrap(); p.next_frame().await };
                        match next {
                            Ok(Some(frame)) => {
                                if frame.format.bit_depth == 16 {
                                    let bytes = &frame.data;
                                    let channels = frame.format.channels as usize;
                                    let mut sample_idx = 0usize;
                                    let mut max0: i32 = 0;
                                    let mut max1: i32 = 0;
                                    let mut i = 0usize;
                                    while i + 1 < bytes.len() {
                                        let sample = i16::from_le_bytes([bytes[i], bytes[i+1]]);
                                        let absv = sample.abs() as i32;
                                        if channels == 1 {
                                            if absv > max0 { max0 = absv; }
                                        } else {
                                            if sample_idx % channels == 0 {
                                                if absv > max0 { max0 = absv; }
                                            } else {
                                                if absv > max1 { max1 = absv; }
                                            }
                                        }
                                        sample_idx += 1;
                                        i += 2;
                                    }
                                    let peak_l = (max0 as f32) / (i16::MAX as f32);
                                    let peak_r = if frame.format.channels > 1 { (max1 as f32) / (i16::MAX as f32) } else { peak_l };
                                    if let Ok(mut pk) = peak_vals_mon.lock() { *pk = (peak_l, peak_r); }
                                }
                            }
                            Ok(None) => { tokio::time::sleep(std::time::Duration::from_millis(15)).await; }
                            Err(_) => break,
                        }
                    }
                });
            });
            self.monitor_stop = Some(monitor_stop);
            self.monitor_thread = Some(handle);
        }
        if self.telemetry_rx.is_none() {
            // attempt to spawn telemetry thread if address provided via env
            if let Ok(addr) = std::env::var("BSM_TELEMETRY_ADDR") {
                let rx = crate::telemetry_client::spawn_telemetry_thread(&addr);
                self.telemetry_rx = Some(rx);
            } else {
                // default address — try localhost:9000
                let rx = crate::telemetry_client::spawn_telemetry_thread("127.0.0.1:9000");
                self.telemetry_rx = Some(rx);
            }
        }

        if let Some(rx) = &self.telemetry_rx {
            // drain all pending messages
            loop {
                match rx.try_recv() {
                    Ok(msg) => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&msg) {
                            if let Some(ev) = v.get("event").and_then(|e| e.as_str()) {
                                match ev {
                                    "audio_stats" => {
                                        if let Some(data) = v.get("data") {
                                            let pl = data;
                                            let peak_l = pl.get("peak_left").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
                                            let peak_r = pl.get("peak_right").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
                                            if let Ok(mut pk) = self.peak_vals.lock() { *pk = (peak_l, peak_r); }
                                        }
                                    }
                                    "audio_started" => { self.logs.push(format!("telemetry: audio_started")); }
                                    "audio_stopped" => { self.logs.push(format!("telemetry: audio_stopped")); }
                                    _ => { /* ignore other events for now */ }
                                }
                            }
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => { self.telemetry_rx = None; break; }

                }
            }
        }

        // Keep egui alive so meters + telemetry update continuously.
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}
