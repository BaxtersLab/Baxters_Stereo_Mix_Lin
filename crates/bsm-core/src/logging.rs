use crate::{BsmError, config::LoggingConfig};
use std::path::Path;
use tracing_subscriber::{fmt, EnvFilter};
use tracing_appender::rolling;

/// Initialize logging/tracing for the application from `LoggingConfig`.
pub fn init_logging(cfg: &LoggingConfig) -> Result<(), BsmError> {
    let filter = EnvFilter::try_new(&cfg.level).unwrap_or_else(|_| EnvFilter::new("info"));

    if cfg.log_to_file {
        let path = Path::new(&cfg.log_file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(BsmError::Io)?;
        }
        let appender = rolling::daily(path.parent().unwrap_or(Path::new(".")), path.file_name().unwrap_or(std::ffi::OsStr::new("bsm.log")));
        let (non_blocking, guard) = tracing_appender::non_blocking(appender);
        fmt()
            .with_env_filter(filter)
            .with_writer(non_blocking)
            .init();
        // Keep guard alive for now by leaking it; real apps should store it.
        std::mem::forget(guard);
    } else {
        fmt().with_env_filter(filter).init();
    }

    Ok(())
}
