use anyhow::Result;
use opentelemetry::{trace::TracerProvider as _, KeyValue};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter, WithExportConfig};
use opentelemetry_sdk::{
    logs::SdkLoggerProvider, metrics::SdkMeterProvider, trace::SdkTracerProvider, Resource,
};
use tracing::info;
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{
    fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer,
};

use crate::setup::LoggingSettings;

/// Guards that must be kept alive for the duration of the application.
/// Dropping these will flush and shutdown the respective logging/tracing systems.
pub struct TracingGuards {
    #[allow(dead_code)]
    file_guard: WorkerGuard,
    tracer_provider: Option<SdkTracerProvider>,
    logger_provider: Option<SdkLoggerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl TracingGuards {
    /// Gracefully shutdown the OpenTelemetry tracer and logger providers.
    /// This ensures all pending spans and logs are flushed before the application exits.
    pub fn shutdown(self) {
        if let Some(tracer_provider) = self.tracer_provider {
            if let Err(e) = tracer_provider.shutdown() {
                eprintln!("Error shutting down tracer provider: {:?}", e);
            }
        }
        if let Some(logger_provider) = self.logger_provider {
            if let Err(e) = logger_provider.shutdown() {
                eprintln!("Error shutting down logger provider: {:?}", e);
            }
        }
        if let Some(meter_provider) = self.meter_provider {
            if let Err(e) = meter_provider.shutdown() {
                eprintln!("Error shutting down meter provider: {:?}", e);
            }
        }
    }
}

struct OtelGuards {
    tracer_provider: Option<SdkTracerProvider>,
    logger_provider: Option<SdkLoggerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

fn get_resource(app_name: &str, app_version: &str, user_id: &str, session_id: &str) -> Resource {
    Resource::builder()
        .with_service_name(app_name.to_string())
        .with_attribute(KeyValue::new("ddb.version", app_version.to_string()))
        .with_attribute(KeyValue::new("user.id", user_id.to_string()))
        .with_attribute(KeyValue::new("session.id", session_id.to_string()))
        .build()
}

fn init_otel_tracer(endpoint: &str, resource: &Resource) -> Result<SdkTracerProvider> {
    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;

    let provider = SdkTracerProvider::builder()
        .with_resource(resource.clone())
        .with_batch_exporter(exporter)
        .build();

    Ok(provider)
}

fn init_otel_logger(endpoint: &str, resource: &Resource) -> Result<SdkLoggerProvider> {
    let exporter = LogExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;

    let provider = SdkLoggerProvider::builder()
        .with_resource(resource.clone())
        .with_batch_exporter(exporter)
        .build();

    Ok(provider)
}

fn init_otel_metrics(endpoint: &str, resource: &Resource) -> Result<SdkMeterProvider> {
    let exporter = MetricExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;

    let provider = SdkMeterProvider::builder()
        .with_resource(resource.clone())
        .with_periodic_exporter(exporter)
        .build();

    Ok(provider)
}

fn init_otel(
    log_settings: &LoggingSettings,
    layers: &mut Vec<Box<dyn Layer<tracing_subscriber::Registry> + Send + Sync>>,
) -> Result<OtelGuards> {
    if log_settings.enable_otel {
        let user_id = match &log_settings.user_id {
            Some(id) => id.clone(),
            None => crate::global::get_user_id().clone(),
        };
        let session_id: String = match &log_settings.session_id {
            Some(id) => id.clone(),
            None => {
                use uuid::Uuid;
                Uuid::new_v4().to_string()
            }
        };
        let resource = get_resource(
            crate::global::APP_NAME,
            crate::global::get_version(),
            &user_id,
            &session_id,
        );

        // Tracer
        let tracer_provider = init_otel_tracer(&log_settings.otel_endpoint, &resource)?;
        let tracer = tracer_provider.tracer("ddb");
        let otel_trace_filter = EnvFilter::new(format!("ddb={}", log_settings.otel_level))
            .add_directive("hyper=off".parse()?)
            .add_directive("tonic=off".parse()?)
            .add_directive("h2=off".parse()?)
            .add_directive("reqwest=off".parse()?);
        let otel_trace_layer = OpenTelemetryLayer::new(tracer)
            .with_filter(otel_trace_filter)
            .boxed();
        layers.push(otel_trace_layer);

        // Logger
        let logger_provider = init_otel_logger(&log_settings.otel_endpoint, &resource)?;
        let otel_log_filter = EnvFilter::new(format!("ddb={}", log_settings.otel_level))
            .add_directive("hyper=off".parse()?)
            .add_directive("tonic=off".parse()?)
            .add_directive("h2=off".parse()?)
            .add_directive("reqwest=off".parse()?);
        let otel_log_layer = OpenTelemetryTracingBridge::new(&logger_provider)
            .with_filter(otel_log_filter)
            .boxed();
        layers.push(otel_log_layer);

        // Metrics
        use tracing_opentelemetry::MetricsLayer;
        let meter_provider = init_otel_metrics(&log_settings.otel_endpoint, &resource)?;
        opentelemetry::global::set_meter_provider(meter_provider.clone());
        let otel_metric_filter = EnvFilter::new(format!("ddb={}", log_settings.otel_level))
            .add_directive("hyper=off".parse()?)
            .add_directive("tonic=off".parse()?)
            .add_directive("h2=off".parse()?)
            .add_directive("reqwest=off".parse()?);
        let otel_metrics_layer = MetricsLayer::new(meter_provider.clone())
            .with_filter(otel_metric_filter)
            .boxed();
        layers.push(otel_metrics_layer);

        Ok(OtelGuards {
            tracer_provider: Some(tracer_provider),
            logger_provider: Some(logger_provider),
            meter_provider: Some(meter_provider),
        })
    } else {
        Ok(OtelGuards {
            tracer_provider: None,
            logger_provider: None,
            meter_provider: None,
        })
    }
}

pub fn setup_logging(
    app_name: &str,
    log_dir: &str,
    log_settings: &LoggingSettings,
) -> Result<TracingGuards> {
    let mut layers: Vec<Box<dyn Layer<tracing_subscriber::Registry> + Send + Sync>> = Vec::new();

    let file_filter = EnvFilter::from_default_env()
        .add_directive(format!("ddb={}", log_settings.file_level).parse()?);

    let console_filter = EnvFilter::from_default_env()
        .add_directive(format!("ddb={}", log_settings.console_level).parse()?);

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

    if log_settings.console_log {
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

    let otel_guards = init_otel(log_settings, &mut layers)?;
    tracing_subscriber::registry().with(layers).try_init()?;
    match &otel_guards.tracer_provider {
        Some(_) => {
            info!(
                "OpenTelemetry tracing enabled. Exporting to {}",
                log_settings.otel_endpoint
            );
        }
        None => {
            info!("OpenTelemetry tracing disabled.");
        }
    }
    match &otel_guards.logger_provider {
        Some(_) => {
            info!(
                "OpenTelemetry logging enabled. Exporting to {}",
                log_settings.otel_endpoint
            );
        }
        None => {
            tracing::info!("OpenTelemetry logging disabled.");
        }
    }
    match &otel_guards.meter_provider {
        Some(_) => {
            info!(
                "OpenTelemetry metrics enabled. Exporting to {}",
                log_settings.otel_endpoint
            );
        }
        None => {
            info!("OpenTelemetry metrics disabled.");
        }
    }

    Ok(TracingGuards {
        file_guard: guard,
        tracer_provider: otel_guards.tracer_provider,
        logger_provider: otel_guards.logger_provider,
        meter_provider: otel_guards.meter_provider,
    })
}
