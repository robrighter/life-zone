//! File + stdout logging. Phase timings go to `tick_stats`, not here; this is
//! for the narrative of a run and for diagnosing startup.

use anyhow::Result;
use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialise logging. The returned guard must be held for the process
/// lifetime — dropping it stops the background writer and loses buffered lines.
pub fn init(log_dir: &Path) -> Result<WorkerGuard> {
    std::fs::create_dir_all(log_dir)?;

    let file_appender = tracing_appender::rolling::daily(log_dir, "life-zone.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_env("LIFE_ZONE_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,life_zone_lib=debug"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true).with_ansi(false).with_writer(file_writer))
        .with(fmt::layer().with_target(false).with_ansi(true))
        .init();

    tracing::info!(dir = %log_dir.display(), "logging started");
    Ok(guard)
}
