use anyhow::Result;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, SpanExporter, WithExportConfig};
use opentelemetry_sdk::{logs::SdkLoggerProvider, trace::SdkTracerProvider, Resource};
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{
    fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer,
};

/// Guards that must be kept alive for the duration of the application.
/// Dropping these will flush and shutdown the respective logging/tracing systems.
pub struct TracingGuards {
    #[allow(dead_code)]
    file_guard: WorkerGuard,
    tracer_provider: SdkTracerProvider,
    logger_provider: SdkLoggerProvider,
}

impl TracingGuards {
    /// Gracefully shutdown the OpenTelemetry tracer and logger providers.
    /// This ensures all pending spans and logs are flushed before the application exits.
    pub fn shutdown(self) {
        if let Err(e) = self.tracer_provider.shutdown() {
            eprintln!("Error shutting down tracer provider: {:?}", e);
        }
        if let Err(e) = self.logger_provider.shutdown() {
            eprintln!("Error shutting down logger provider: {:?}", e);
        }
    }
}

fn get_resource() -> Resource {
    Resource::builder().with_service_name("ddb").build()
}

fn init_otel_tracer(endpoint: &str) -> Result<SdkTracerProvider> {
    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;

    let provider = SdkTracerProvider::builder()
        .with_resource(get_resource())
        .with_batch_exporter(exporter)
        .build();

    Ok(provider)
}

fn init_otel_logger(endpoint: &str) -> Result<SdkLoggerProvider> {
    let exporter = LogExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;

    let provider = SdkLoggerProvider::builder()
        .with_resource(get_resource())
        .with_batch_exporter(exporter)
        .build();

    Ok(provider)
}

pub fn setup_logging(
    app_name: &str,
    log_dir: &str,
    enable_console_logging: bool,
    console_level: &str,
    file_level: &str,
    otel_endpoint: &str,
    otel_level: &str,
) -> Result<TracingGuards> {
    let mut layers = Vec::new();

    let file_filter =
        EnvFilter::from_default_env().add_directive(format!("ddb={}", file_level).parse()?);

    let console_filter =
        EnvFilter::from_default_env().add_directive(format!("ddb={}", console_level).parse()?);

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

    // Initialize OpenTelemetry tracer and add the layer
    let tracer_provider = init_otel_tracer(otel_endpoint)?;
    let tracer = tracer_provider.tracer("ddb");
    let otel_trace_filter = EnvFilter::new(format!("ddb={}", otel_level))
        .add_directive("hyper=off".parse()?)
        .add_directive("tonic=off".parse()?)
        .add_directive("h2=off".parse()?)
        .add_directive("reqwest=off".parse()?);
    let otel_trace_layer = OpenTelemetryLayer::new(tracer)
        .with_filter(otel_trace_filter)
        .boxed();
    layers.push(otel_trace_layer);

    // Initialize OpenTelemetry logger and add the bridge layer
    let logger_provider = init_otel_logger(otel_endpoint)?;
    let otel_level = otel_level;
    // Filter to prevent telemetry-induced-telemetry loops (hyper/tonic/h2 are used by OTLP exporter)
    let otel_log_filter = EnvFilter::new(format!("ddb={}", otel_level))
        .add_directive("hyper=off".parse()?)
        .add_directive("tonic=off".parse()?)
        .add_directive("h2=off".parse()?)
        .add_directive("reqwest=off".parse()?);
    let otel_log_layer = OpenTelemetryTracingBridge::new(&logger_provider)
        .with_filter(otel_log_filter)
        .boxed();
    layers.push(otel_log_layer);

    tracing_subscriber::registry().with(layers).try_init()?;

    Ok(TracingGuards {
        file_guard: guard,
        tracer_provider,
        logger_provider,
    })
}
