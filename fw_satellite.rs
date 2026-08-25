// fw_satellite.rs — FullWrapper satellite for Rust apps.
// Self-contained: stdlib only. No external crate dependencies.
// Deployed to each Rust app in the ecosystem.
// Replace APP_CODE with the specific app identifier.
//
// To run: rustc fw_satellite.rs -o fw-satellite-BSM && .\fw-satellite-BSM

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const APP_CODE: &str = "BSM";
const PIPE_PATH: &str = r"\\.\pipe\fw-satellite-BSM";
const FALLBACK_REPORT: &str = ".fw_satellite_report.json";

fn dir_size(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

fn count_matching(root: &Path, ext: &str) -> u64 {
    if !root.exists() {
        return 0;
    }
    let mut count = 0u64;
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                count += count_matching(&p, ext);
            } else if p.extension().and_then(|e| e.to_str()) == Some(ext) {
                count += 1;
            }
        }
    }
    count
}

fn unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn build_json(
    app_code: &str,
    ts: u64,
    target_bytes: u64,
    log_count: u64,
    temp_count: u64,
) -> String {
    format!(
        r#"{{"app_code":"{app_code}","timestamp":{ts},"target_size_bytes":{target_bytes},"log_file_count":{log_count},"temp_file_count":{temp_count}}}"#
    )
}

fn collect_and_send(app_root: &Path) {
    let target = app_root.join("target");
    let logs_dir = app_root.join("logs");

    let target_bytes = dir_size(&target);
    let log_count = count_matching(&logs_dir, "log");
    let temp_count =
        count_matching(app_root, "tmp") + count_matching(app_root, "bak");
    let ts = unix_ts();

    let json = build_json(APP_CODE, ts, target_bytes, log_count, temp_count);
    let payload = json.as_bytes();

    // Try named pipe first, fall back to local file.
    let pipe = PathBuf::from(PIPE_PATH);
    if let Ok(mut f) = fs::OpenOptions::new().write(true).open(&pipe) {
        let _ = f.write_all(payload);
        return;
    }
    let fallback = app_root.join(FALLBACK_REPORT);
    if let Ok(mut f) = fs::File::create(fallback) {
        let _ = f.write_all(payload);
    }
}

fn main() {
    let app_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    collect_and_send(&app_root);
}
