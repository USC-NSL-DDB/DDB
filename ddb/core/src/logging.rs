use anyhow::Result;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::{trace::SdkTracerProvider, Resource};
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
}

impl TracingGuards {
    /// Gracefully shutdown the OpenTelemetry tracer provider.
    /// This ensures all pending spans are flushed before the application exits.
    pub fn shutdown(self) {
        if let Err(e) = self.tracer_provider.shutdown() {
            eprintln!("Error shutting down tracer provider: {:?}", e);
        }
    }
}

fn init_otel_tracer(endpoint: &str) -> Result<SdkTracerProvider> {
    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;

    let resource = Resource::builder().with_service_name("ddb").build();

    let provider = SdkTracerProvider::builder()
        .with_resource(resource)
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
    let otel_layer = OpenTelemetryLayer::new(tracer)
        .with_filter(EnvFilter::new("ddb=info"))
        .boxed();
    layers.push(otel_layer);

    tracing_subscriber::registry().with(layers).try_init()?;

    Ok(TracingGuards {
        file_guard: guard,
        tracer_provider,
    })
}
