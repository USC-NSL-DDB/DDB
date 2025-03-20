use anyhow::Result;
use std::str::FromStr;
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::{
    fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer,
};

pub fn setup_logging(
    app_name: &str,
    log_dir: &str,
    enable_console_logging: bool,
    console_level: &str,
    file_level: &str,
) -> Result<WorkerGuard> {
    // Set up file appender
    let file_appender =
        RollingFileAppender::new(Rotation::DAILY, log_dir, format!("{}.log", app_name));

    // Create a non-blocking writer (async log writing)
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let console_filter = EnvFilter::from_str(&format!("ddb={}", console_level))?;
    let file_filter = EnvFilter::from_str(&format!("ddb={}", file_level))?;

    // Console Layer with color support and pretty formatting
    let console_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_file(true)
        .with_line_number(true)
        .with_span_events(FmtSpan::CLOSE)
        .with_ansi(true)
        .with_filter(console_filter);

    // File Layer with JSON formatting
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_file(true)
        .with_line_number(true)
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(non_blocking) // Use non-blocking writer
        .with_filter(file_filter);

    let t = tracing_subscriber::registry().with(file_layer);

    if enable_console_logging {
        t.with(console_layer).try_init()?;
    } else {
        t.try_init()?;
    }

    Ok(guard)
}
