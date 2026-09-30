use axum::{routing::{get, post}, Router, Json, extract::State};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{info, error, warn};

mod k8s;

#[derive(Clone)]
pub struct AppState {
    pub k8s_client: kube::Client,
    pub namespace: String,
    pub agent_runner_image: String,
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
    tracing_subscriber::fmt::init();

    let k8s_client = kube::Client::try_default().await?;
    let namespace = std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "aramaki".to_string());
    let agent_runner_image = std::env::var("AGENT_RUNNER_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/jzacharie/opencode-agent:latest".to_string());

    info!("Initialisation d'Aramaki (Chief Section 9) dans le namespace '{}'", namespace);

    let state = Arc::new(AppState {
        k8s_client,
        namespace,
        agent_runner_image,
    });

    let app = Router::new()
        .route("/slack/events", post(handle_slack_event))
        .route("/health", get(|| async { "OK - Aramaki active" }))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    info!("Orchestrateur Aramaki à l'écoute sur http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
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

    if let Some(event) = payload.event {
        if event.r#type == "app_mention" || event.r#type == "message" {
            let state_clone = state.clone();
            tokio::spawn(async move {
                if let Err(e) = process_agent_request(state_clone, event).await {
                    error!("Erreur traitement agent: {:?}", e);
                }
            });
        }
    }

    Ok(Json(serde_json::json!({ "status": "ok" })))
}

async fn process_agent_request(state: Arc<AppState>, event: SlackEventDetail) -> Result<(), Box<dyn std::error::Error>> {
    let parts: Vec<&str> = event.text.split_whitespace().collect();
    let agent_name = parts.iter()
        .find(|p| p.starts_with("agent-"))
        .map(|s| *s)
        .unwrap_or("agent-code-reviewer");

    info!("Reçu demande pour l'agent '{}' sur le canal {}", agent_name, event.channel);

    let exists = k8s::check_agent_configmap_exists(&state.k8s_client, &state.namespace, agent_name).await?;
    if !exists {
        warn!("ConfigMap introuvable pour l'agent {}", agent_name);
        return Ok(());
    }

    k8s::spawn_agent_job(
        &state.k8s_client,
        &state.namespace,
        &state.agent_runner_image,
        agent_name,
        &event.text,
        &event.channel,
        &event.ts,
    ).await?;

    Ok(())
}
