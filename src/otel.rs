//! Module OpenTelemetry OTLP pour exporter les traces et métriques d'Amaraki vers OpenObserve.
//!
//! Variables d'environnement :
//! - OTEL_EXPORTER_OTLP_ENDPOINT : Endpoint OTLP HTTP (défaut : "http://o2-openobserve-router.openobserve.svc.cluster.local:5080/api/default")
//! - OPENOBSERVE_AUTH : Header Authorization (défaut : "Basic bGxtLmRlZmF1bHRAc2EuaW50ZXJuYWw6S2hlbm8xUTNXTGl6UzYyWg==")
//! - OTEL_SERVICE_NAME : Nom du service (défaut : "amaraki")

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::trace::SdkTracerProvider;
use std::collections::HashMap;

pub fn setup_otel() -> Option<SdkTracerProvider> {
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://o2-openobserve-router.openobserve.svc.cluster.local:5080/api/default".to_string());

    let auth_header = std::env::var("OPENOBSERVE_AUTH")
        .unwrap_or_else(|_| "Basic bGxtLmRlZmF1bHRAc2EuaW50ZXJuYWw6S2hlbm8xUTNXTGl6UzYyWg==".to_string());

    let service_name = std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "amaraki".to_string());

    let traces_url = format!("{}/v1/traces", endpoint.trim_end_matches('/'));

    eprintln!(
        "[amaraki] 📡 OpenTelemetry OTLP — Initialisation de l'export vers OpenObserve (endpoint: {}, service: {})",
        traces_url, service_name
    );

    let mut headers = HashMap::new();
    headers.insert("Authorization".to_string(), auth_header);

    let exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(traces_url)
        .with_headers(headers)
        .build()
    {
        Ok(exp) => exp,
        Err(e) => {
            eprintln!("[amaraki] ⚠️ Échec de la construction de l'exporteur OTLP : {}", e);
            return None;
        }
    };

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .build();

    let _tracer = provider.tracer(service_name);
    opentelemetry::global::set_tracer_provider(provider.clone());

    eprintln!("[amaraki] ✅ OpenTelemetry OTLP — Export vers OpenObserve activé");
    Some(provider)
}
