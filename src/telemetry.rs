use std::{
    env,
    sync::OnceLock,
    time::Duration,
};

use opentelemetry::{
    global,
    metrics::{Counter, Histogram},
    trace::TracerProvider as _,
    KeyValue,
};
use opentelemetry_otlp::{MetricExporter, Protocol, SpanExporter, WithExportConfig};
use opentelemetry_sdk::{
    metrics::SdkMeterProvider,
    trace::SdkTracerProvider,
    Resource,
};
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _, EnvFilter};

const SERVICE_NAME: &str = "cyeam-api";

struct RequestMetrics {
    duration: Histogram<f64>,
    failures: Counter<u64>,
    requests: Counter<u64>,
}

static REQUEST_METRICS: OnceLock<RequestMetrics> = OnceLock::new();

pub struct Telemetry {
    meter_provider: SdkMeterProvider,
    tracer_provider: SdkTracerProvider,
}

/// Installs OpenTelemetry only when an OTLP endpoint is configured. This keeps
/// local development and tests free of an exporter dependency while production
/// exports service telemetry to New Relic through standard OTLP/HTTP.
pub fn init() -> Option<Telemetry> {
    let endpoint = env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .filter(|value| !value.is_empty());
    let resource = Resource::builder()
        .with_service_name(env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| SERVICE_NAME.to_owned()))
        .with_attributes([
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            KeyValue::new("deployment.environment.name", env::var("OTEL_DEPLOYMENT_ENVIRONMENT").unwrap_or_else(|_| "production".to_owned())),
        ])
        .build();

    let telemetry = endpoint.map(|endpoint| {
        let endpoint = endpoint.trim_end_matches('/');
        let span_exporter = SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(format!("{endpoint}/v1/traces"))
            .build()
            .expect("build OpenTelemetry OTLP exporter");
        let tracer_provider = SdkTracerProvider::builder()
            .with_batch_exporter(span_exporter)
            .with_resource(resource.clone())
            .build();
        global::set_tracer_provider(tracer_provider.clone());

        let metric_exporter = MetricExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(format!("{endpoint}/v1/metrics"))
            .build()
            .expect("build OpenTelemetry OTLP metric exporter");
        let meter_provider = SdkMeterProvider::builder()
            .with_periodic_exporter(metric_exporter)
            .with_resource(resource)
            .build();
        global::set_meter_provider(meter_provider.clone());
        install_request_metrics();

        Telemetry {
            meter_provider,
            tracer_provider,
        }
    });

    let telemetry_layer = telemetry.as_ref().map(|telemetry| {
        tracing_opentelemetry::layer().with_tracer(telemetry.tracer_provider.tracer(SERVICE_NAME))
    });
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .with(telemetry_layer)
        .init();

    telemetry
}

impl Telemetry {
    pub fn shutdown(self) {
        let _ = self.tracer_provider.shutdown();
        let _ = self.meter_provider.shutdown();
    }
}

pub fn record_request(method: &str, route: &str, status: u16, duration: Duration) {
    let Some(metrics) = REQUEST_METRICS.get() else {
        return;
    };
    let attributes = [
        KeyValue::new("http.request.method", method.to_owned()),
        KeyValue::new("http.route", route.to_owned()),
        KeyValue::new("http.response.status_code", i64::from(status)),
    ];
    metrics.requests.add(1, &attributes);
    metrics.duration.record(duration.as_secs_f64(), &attributes);
    if status >= 500 {
        metrics.failures.add(1, &attributes);
    }
}

fn install_request_metrics() {
    let meter = global::meter(SERVICE_NAME);
    let _ = REQUEST_METRICS.set(RequestMetrics {
        duration: meter
            .f64_histogram("http.server.request.duration")
            .with_unit("s")
            .build(),
        failures: meter.u64_counter("http.server.request.errors").build(),
        requests: meter.u64_counter("http.server.request.count").build(),
    });
}
