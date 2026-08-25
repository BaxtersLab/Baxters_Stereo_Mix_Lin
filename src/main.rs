use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use anyhow::Result;
use bsm_core::{config::load_config, config::validate_config, logging::init_logging, instance::SingleInstanceGuard, app::AppState};
use tokio::sync::broadcast;
use bsm_ipc::telemetry::TelemetryEmitter;
use bsm_hrt::start_hrt_client;
use bsm_hrt::bridge::spawn_bridge;

fn main() -> Result<()> {
    // 1. Single-instance guard
    let _guard = SingleInstanceGuard::acquire().map_err(|e| anyhow::anyhow!("{}", e))?;

    // 2. Load config (creates default if missing)
    let config = load_config()?;

    // 3. Validate — log warnings, do not abort on minor issues
    let warnings = validate_config(&config);
    for w in &warnings {
        eprintln!("Config warning: {}", w);
    }

    // 4. Init logging
    init_logging(&config.logging)?;
    tracing::info!("Baxter's Stereo Mix starting (v{})", env!("CARGO_PKG_VERSION"));

    // 5. Shared app state
    let app_state = Arc::new(AppState::new(config));

    // 6. Tokio runtime
    let runtime = tokio::runtime::Runtime::new()?;

    runtime.block_on(async move {
        // Telemetry channel (shared by subsystems)
        let (telemetry_tx, _telemetry_rx) = broadcast::channel::<String>(32);
        let telemetry = TelemetryEmitter::new(telemetry_tx.clone());

        // Start HRT client (health & regulation transport)
        let state_rx = app_state.recording_state_rx();
        let hrt_handle = start_hrt_client(app_state.clone(), state_rx, telemetry.clone(), app_state.shutdown_rx()).await;

        // Spawn the HRT bridge to route inbound HRT messages into subsystems
        let _bridge_handle = spawn_bridge(
            app_state.clone(),
            telemetry.clone(),
            hrt_handle.inbound_rx,
            app_state.shutdown_rx(),
        );

        // Start a TCP IPC server for telemetry clients (UI). Listen on localhost:9000.
        // Use a shared AtomicBool to request the listener shut down when app shutdown occurs.
        let tcp_shutdown = Arc::new(AtomicBool::new(false));
        let tcp_shutdown_clone = tcp_shutdown.clone();
        let telemetry_for_tcp = telemetry.clone();
        let tcp_addr = "127.0.0.1:9000".to_string();
        tokio::spawn(async move {
            if let Err(e) = bsm_ipc::server::run_tcp_server(&tcp_addr, telemetry_for_tcp, tcp_shutdown_clone).await {
                tracing::error!(%e, "tcp ipc server failure");
            }
        });

        // When the app signals shutdown, set the tcp_shutdown flag so the TCP listener exits.
        let app_state_clone_for_tcp = app_state.clone();
        let tcp_shutdown_setter = tcp_shutdown.clone();
        tokio::spawn(async move {
            let mut rx = app_state_clone_for_tcp.shutdown_rx();
            let _ = rx.recv().await;
            tcp_shutdown_setter.store(true, Ordering::SeqCst);
        });

        // Ctrl+C / SIGTERM → graceful shutdown
        let shutdown_state = app_state.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Shutdown signal received");
            shutdown_state.initiate_shutdown();
        });

        let mut shutdown_rx = app_state.shutdown_rx();
        let _ = shutdown_rx.recv().await;
        tracing::info!("Baxter's Stereo Mix shut down cleanly");
    });

    Ok(())
}
