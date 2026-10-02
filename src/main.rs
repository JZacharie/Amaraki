use axum::{
    extract::State,
    middleware,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{error, info, warn};

mod auth;
mod k8s;
mod metrics;
mod web;

use auth::{require_auth_middleware, AuthConfig};
use metrics::MetricsStore;
use web::WebState;

#[derive(Clone)]
pub struct AppState {
    pub k8s_client: Option<kube::Client>,
    pub namespace: String,
    pub agent_runner_image: String,
    pub metrics: Arc<MetricsStore>,
    pub auth: Arc<AuthConfig>,
}

#[derive(Deserialize, Debug)]
pub struct SlackEventPayload {
    pub token: Option<String>,
    pub team_id: Option<String>,
    pub api_app_id: Option<String>,
    pub event: Option<SlackEventDetail>,
    pub r#type: String,
    pub challenge: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct SlackEventDetail {
    pub r#type: String,
    pub channel: String,
    pub user: Option<String>,
    pub text: String,
    pub ts: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,aramaki=info".into());
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .init();

    info!("=== Démarrage d'Aramaki (Chief Section 9 Agent Orchestrator & Observability) ===");

    let namespace = std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "aramaki".to_string());
    let agent_runner_image = std::env::var("AGENT_RUNNER_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/jzacharie/opencode-agent:latest".to_string());

    let metrics = Arc::new(MetricsStore::new());
    let auth = Arc::new(AuthConfig::from_env());

    // Tentative de connexion au cluster Kubernetes
    let k8s_client = match kube::Client::try_default().await {
        Ok(client) => {
            info!(
                "[ACTION] 🔌 Connexion réussie à l'API Kubernetes dans le namespace '{}'",
                namespace
            );
            metrics.set_k8s_connected(true);

            // Découverte initiale des agents déclarés dans les ConfigMaps
            match k8s::discover_all_agents(&client, &namespace, &metrics).await {
                Ok(count) => {
                    info!(
                        "[ACTION] 🔍 Découverte K8s: {} agent(s) découvert(s) depuis les ConfigMaps Kubernetes",
                        count
                    );
                    if count == 0 {
                        k8s::seed_default_agents(&metrics).await;
                    }
                }
                Err(e) => {
                    warn!("[ACTION] ⚠️ Impossible de lister les ConfigMaps ({}), chargement des agents par défaut", e);
                    k8s::seed_default_agents(&metrics).await;
                }
            }

            Some(client)
        }
        Err(e) => {
            warn!(
                "[ACTION] ⚠️ API Kubernetes non disponible (mode autonome/hors cluster : {}) - Mode simulé actif",
                e
            );
            metrics.set_k8s_connected(false);
            k8s::seed_default_agents(&metrics).await;
            None
        }
    };

    let state = Arc::new(AppState {
        k8s_client: k8s_client.clone(),
        namespace: namespace.clone(),
        agent_runner_image: agent_runner_image.clone(),
        metrics: metrics.clone(),
        auth: auth.clone(),
    });

    let web_state = Arc::new(WebState {
        metrics: metrics.clone(),
        auth: auth.clone(),
        namespace: namespace.clone(),
        k8s_client: k8s_client.clone(),
        agent_runner_image: agent_runner_image.clone(),
    });

    // Tâche d'arrière-plan : synchronisation périodique des Jobs Kubernetes et statut
    let bg_metrics = metrics.clone();
    let bg_k8s = k8s_client.clone();
    let bg_ns = namespace.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            if let Some(ref client) = bg_k8s {
                if let Err(e) = k8s::sync_jobs(client, &bg_ns, &bg_metrics).await {
                    debug_log_sync_error(&e);
                }
            }
        }
    });

    // 1. Routes protégées par authentification (Dashboard & APIs de contrôle)
    let auth_layer_clone = auth.clone();
    let protected_routes = Router::new()
        .route("/", get(web::dashboard_html_handler))
        .route("/dashboard", get(web::dashboard_html_handler))
        .route("/api/dashboard/stats", get(web::dashboard_stats_handler))
        .route("/api/auth/logout", post(web::logout_handler))
        .route("/api/auth/me", get(web::auth_me_handler))
        .route("/api/agents/test-trigger", post(web::test_trigger_handler))
        .route("/api/agents/callback", post(web::agent_callback_handler))
        .route_layer(middleware::from_fn(move |req, next| {
            let auth = auth_layer_clone.clone();
            require_auth_middleware(auth, req, next)
        }))
        .with_state(web_state.clone());

    // 2. Routes publiques & endpoints d'ingestion/métriques
    let public_routes = Router::new()
        .route("/health", get(|| async { "OK - Aramaki active" }))
        .route("/login", get(web::login_html_handler))
        .route("/api/auth/login", post(web::login_handler))
        .route("/metrics", get(web::prometheus_metrics_handler))
        .route(
            "/api/otel/v1/metrics",
            get(web::opentelemetry_metrics_handler),
        )
        .with_state(web_state.clone());

    // 3. Route Slack Event Ingestion
    let slack_route = Router::new()
        .route("/slack/events", post(handle_slack_event))
        .with_state(state.clone());

    // Assemblage de l'application avec Access Logger global
    let auth_log_clone = auth.clone();
    let app = Router::new()
        .merge(public_routes)
        .merge(slack_route)
        .merge(protected_routes)
        .layer(middleware::from_fn(move |req, next| {
            let auth = auth_log_clone.clone();
            auth::access_log_middleware(auth, req, next)
        }));

    let host = std::env::var("ARAMAKI_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = std::env::var("PORT")
        .or_else(|_| std::env::var("ARAMAKI_PORT"))
        .unwrap_or_else(|_| "3000".to_string())
        .parse::<u16>()
        .unwrap_or(3000);

    let addr = SocketAddr::new(host.parse()?, port);
    info!(
        "[ACTION] 🌐 Interface Web & Orchestrateur Aramaki accessibles sur http://{}",
        addr
    );
    info!("Dashboard Web sécurisé : http://{}/", addr);
    info!("Endpoint Prometheus : http://{}/metrics", addr);
    info!(
        "Endpoint OpenTelemetry : http://{}/api/otel/v1/metrics",
        addr
    );

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

fn debug_log_sync_error(e: &kube::Error) {
    tracing::debug!("Sync jobs K8s: {}", e);
}

async fn handle_slack_event(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SlackEventPayload>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    // Challenge Slack URL verification
    if payload.r#type == "url_verification" {
        if let Some(challenge) = payload.challenge {
            return Ok(Json(serde_json::json!({ "challenge": challenge })));
        }
    }

    state.metrics.record_request(true);

    if let Some(event) = payload.event {
        if event.r#type == "app_mention" || event.r#type == "message" {
            let prompt_preview = if event.text.len() > 80 {
                format!("{}...", &event.text[..77])
            } else {
                event.text.clone()
            };
            info!(
                "[ACTION] 📩 Événement Slack reçu (canal: '{}', user: '{:?}') - Extrait: \"{}\"",
                event.channel, event.user, prompt_preview
            );
            let state_clone = state.clone();
            tokio::spawn(async move {
                if let Err(e) = process_agent_request(state_clone, event).await {
                    error!("[ACTION] 💥 Erreur traitement agent: {:?}", e);
                }
            });
        }
    }

    Ok(Json(serde_json::json!({ "status": "ok" })))
}

async fn process_agent_request(
    state: Arc<AppState>,
    event: SlackEventDetail,
) -> Result<(), Box<dyn std::error::Error>> {
    let parts: Vec<&str> = event.text.split_whitespace().collect();
    let agent_name = parts
        .iter()
        .find(|p| p.starts_with("agent-"))
        .copied()
        .unwrap_or("agent-code-reviewer");

    info!(
        "[ACTION] 🎯 Requête agent Slack: Agent '{}' sollicité pour le canal '{}'",
        agent_name, event.channel
    );

    if let Some(ref client) = state.k8s_client {
        let exists =
            k8s::check_agent_configmap_exists(client, &state.namespace, agent_name).await?;
        if !exists {
            warn!(
                "[ACTION] ⚠️ Déploiement refusé: ConfigMap introuvable pour l'agent '{}' dans le namespace '{}'",
                agent_name, state.namespace
            );
            state.metrics.record_request(false);
            return Ok(());
        }

        k8s::spawn_agent_job(
            client,
            &state.namespace,
            &state.agent_runner_image,
            agent_name,
            &event.text,
            &event.channel,
            &event.ts,
            &state.metrics,
        )
        .await?;
    } else {
        info!(
            "[ACTION] 🤖 Mode autonome : simulation du lancement de l'agent {}",
            agent_name
        );
        let sim_id = format!(
            "{}-sim-{}",
            agent_name,
            &uuid::Uuid::new_v4().to_string()[..8]
        );
        state
            .metrics
            .record_agent_spawn(
                &sim_id,
                agent_name,
                "opencode/free-default-model",
                &event.channel,
                &event.text,
            )
            .await;
    }

    Ok(())
}
