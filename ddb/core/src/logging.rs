use anyhow::Result;
use std::str::FromStr;
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::{
    EnvFilter, Layer, filter::Targets, fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt
};

pub fn setup_logging(
    app_name: &str,
    log_dir: &str,
    enable_console_logging: bool,
    console_level: &str,
    file_level: &str,
) -> Result<WorkerGuard> {
    let mut layers = Vec::new();

    let file_filter = EnvFilter::from_default_env()
        .add_directive(format!("ddb={}", file_level).parse()?);

    let console_filter = EnvFilter::from_default_env()
        .add_directive(format!("ddb={}", console_level).parse()?);

    // Create a non-blocking writer (async log writing) for file logging
    let file_appender =
        RollingFileAppender::new(Rotation::DAILY, log_dir, format!("{}.log", app_name));
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_file(true)
        .with_line_number(true)
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(non_blocking) // Use non-blocking writer
        .with_filter(file_filter)
        .boxed();
    layers.push(file_layer);

    if enable_console_logging {
        // Console Layer with color support and pretty formatting
        let console_layer = tracing_subscriber::fmt::layer()
            .with_target(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_file(true)
            .with_line_number(true)
            .with_span_events(FmtSpan::CLOSE)
            .with_ansi(true)
            .with_filter(console_filter)
            .boxed();
        layers.push(console_layer);
    } 

    tracing_subscriber::registry().with(layers).try_init()?;
    Ok(guard)
}
