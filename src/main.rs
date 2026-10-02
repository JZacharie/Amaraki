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
mod gatekeeper;
mod k8s;
mod metrics;
mod slack;
mod web;

use auth::{require_auth_middleware, AuthConfig};
use gatekeeper::{GatekeeperStore, PendingValidation, UserValidationIntent};
use metrics::MetricsStore;
use slack::SlackNotifier;
use web::WebState;

#[derive(Clone)]
pub struct AppState {
    pub k8s_client: Option<kube::Client>,
    pub namespace: String,
    pub agent_runner_image: String,
    pub metrics: Arc<MetricsStore>,
    pub auth: Arc<AuthConfig>,
    pub slack_notifier: Arc<SlackNotifier>,
    pub gatekeeper: Arc<GatekeeperStore>,
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

#[derive(Deserialize, Debug, Clone)]
pub struct SlackEventDetail {
    pub r#type: String,
    pub channel: String,
    pub user: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    pub ts: String,
    pub thread_ts: Option<String>,
    #[serde(default)]
    pub files: Vec<SlackFileDetail>,
    pub bot_id: Option<String>,
    pub subtype: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct SlackFileDetail {
    pub id: String,
    pub name: Option<String>,
    pub mimetype: Option<String>,
    pub filetype: Option<String>,
    pub url_private_download: Option<String>,
    pub url_private: Option<String>,
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

    let slack_notifier = Arc::new(SlackNotifier::new());
    let gatekeeper = Arc::new(GatekeeperStore::new());

    let state = Arc::new(AppState {
        k8s_client: k8s_client.clone(),
        namespace: namespace.clone(),
        agent_runner_image: agent_runner_image.clone(),
        metrics: metrics.clone(),
        auth: auth.clone(),
        slack_notifier: slack_notifier.clone(),
        gatekeeper: gatekeeper.clone(),
    });

    let web_state = Arc::new(WebState {
        metrics: metrics.clone(),
        auth: auth.clone(),
        namespace: namespace.clone(),
        k8s_client: k8s_client.clone(),
        agent_runner_image: agent_runner_image.clone(),
        slack_notifier: slack_notifier.clone(),
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
        .route("/api/agents/:name/config", get(web::agent_config_handler))
        .route("/api/auth/logout", post(web::logout_handler))
        .route("/api/auth/me", get(web::auth_me_handler))
        .route("/api/agents/test-trigger", post(web::test_trigger_handler))
        .route("/api/agents/callback", post(web::agent_callback_handler))
        .route("/api/slack/send", post(web::slack_send_handler))
        .route_layer(middleware::from_fn(move |req, next| {
            let auth = auth_layer_clone.clone();
            require_auth_middleware(auth, req, next)
        }))
        .with_state(web_state.clone());

    // 2. Routes publiques & endpoints d'ingestion/métriques
    let public_routes = Router::new()
        .route("/health", get(|| async { "OK - Aramaki active" }))
        .route("/logo.png", get(web::logo_handler))
        .route("/favicon.ico", get(web::logo_handler))
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
        // Ignorer les messages générés par les bots pour éviter toute boucle infinie
        if event.bot_id.is_some() || event.subtype.as_deref() == Some("bot_message") {
            return Ok(Json(serde_json::json!({ "status": "ignored_bot_message" })));
        }

        let user_id = event.user.as_deref().unwrap_or("inconnu");
        let text_content = event.text.clone().unwrap_or_default();
        let channel_type = if event.channel.starts_with('D') {
            "DM"
        } else {
            "Canal"
        };

        info!(
            "[SLACK] 📥 [{}] Message reçu de '{}' sur {} (thread: {:?}): \"{}\"",
            channel_type, user_id, event.channel, event.thread_ts, text_content
        );

        // Détection de fichiers audio ou vidéo
        let has_audio_video = event.files.iter().any(|f| {
            let mime = f.mimetype.as_deref().unwrap_or("");
            let name = f.name.as_deref().unwrap_or("");
            mime.starts_with("audio/")
                || mime.starts_with("video/")
                || name.ends_with(".mp3")
                || name.ends_with(".wav")
                || name.ends_with(".m4a")
                || name.ends_with(".ogg")
                || name.ends_with(".mp4")
                || name.ends_with(".mov")
                || name.ends_with(".webm")
        });

        let should_trigger = state
            .gatekeeper
            .should_trigger(
                &event.r#type,
                &text_content,
                &event.channel,
                event.thread_ts.as_deref(),
                has_audio_video,
            )
            .await;

        if should_trigger {
            let prompt_preview = if text_content.len() > 80 {
                format!("{}...", &text_content[..77])
            } else {
                text_content.clone()
            };
            info!(
                "[ACTION] 📩 Déclencheur Amaraki activé (canal: '{}', user: '{}') - \"{}\"",
                event.channel, user_id, prompt_preview
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

fn strip_slack_mentions(text: &str) -> String {
    let mut result = String::new();
    let mut in_mention = false;
    for c in text.chars() {
        if c == '<' {
            in_mention = true;
        } else if in_mention && c == '>' {
            in_mention = false;
        } else if !in_mention {
            result.push(c);
        }
    }
    result.trim().to_string()
}

async fn process_agent_request(
    state: Arc<AppState>,
    event: SlackEventDetail,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let thread_id = event.thread_ts.clone().unwrap_or_else(|| event.ts.clone());
    let session_key = GatekeeperStore::make_key(&event.channel, Some(&thread_id));

    let mut instruction_text = event.text.clone().unwrap_or_default();

    // 1. Traitement des médias (fichiers audio ou vidéo)
    let mut has_audio = false;
    for file in &event.files {
        let mime = file.mimetype.as_deref().unwrap_or("");
        let name = file.name.as_deref().unwrap_or("media.mp3");
        if mime.starts_with("audio/")
            || mime.starts_with("video/")
            || name.ends_with(".mp3")
            || name.ends_with(".wav")
            || name.ends_with(".m4a")
        {
            has_audio = true;
            if let Some(download_url) = file
                .url_private_download
                .as_ref()
                .or(file.url_private.as_ref())
            {
                let slack_token = std::env::var("SLACK_BOT_TOKEN").ok();
                state
                    .slack_notifier
                    .post_message(
                        &event.channel,
                        "🎙️ *Chef Amaraki* : Fichier audio/vidéo détecté. Récupération du flux et transcription en cours...",
                        Some(&thread_id),
                    )
                    .await;

                match state
                    .gatekeeper
                    .transcribe_slack_media(download_url, slack_token.as_deref(), name)
                    .await
                {
                    Ok(transcription) => {
                        instruction_text = transcription;
                        let notice =
                            format!("📝 *Transcription vocale* : « {} »", instruction_text);
                        state
                            .slack_notifier
                            .post_message(&event.channel, &notice, Some(&thread_id))
                            .await;
                    }
                    Err(e) => {
                        error!("[GATEKEEPER] ❌ Erreur transcription audio: {:?}", e);
                        let fail_msg = "⚠️ *Chef Amaraki* : L'analyse audio a échoué (impossible de transcrire l'enregistrement). Aucune action n'est entreprise ni interprétée.";
                        state
                            .slack_notifier
                            .post_message(&event.channel, fail_msg, Some(&thread_id))
                            .await;
                        // On interrompt immédiatement sans chercher à interpréter
                        return Ok(());
                    }
                }
                break;
            } else {
                let fail_msg = "⚠️ *Chef Amaraki* : Impossible d'accéder au fichier audio joint (URL de téléchargement manquant).";
                state
                    .slack_notifier
                    .post_message(&event.channel, fail_msg, Some(&thread_id))
                    .await;
                return Ok(());
            }
        }
    }

    // Nettoyage de l'instruction (suppression des mentions Slack <@...>)
    let cleaned = strip_slack_mentions(&instruction_text);
    if cleaned.is_empty() {
        if has_audio {
            let fail_msg = "⚠️ *Chef Amaraki* : Aucun contenu vocal n'a pu être extrait du fichier audio. Aucune action n'est entreprise.";
            state
                .slack_notifier
                .post_message(&event.channel, fail_msg, Some(&thread_id))
                .await;
        } else {
            info!("[ACTION] Aucun texte ni instruction exploitable dans l'événement");
        }
        return Ok(());
    }
    instruction_text = cleaned;

    // 2. Étape Gatekeeper : Vérification si une validation est en attente
    if let Some(pending) = state.gatekeeper.get_pending(&session_key).await {
        match GatekeeperStore::parse_validation(&instruction_text) {
            UserValidationIntent::Yes => {
                // "Oui : Lancement de l'action."
                state.gatekeeper.remove_pending(&session_key).await;
                let launch_msg = format!(
                    "🚀 *Chef Amaraki* : Accord reçu. Lancement immédiat de l'action avec l'agent *{}*...",
                    pending.agent_name
                );
                state
                    .slack_notifier
                    .post_message(&event.channel, &launch_msg, Some(&thread_id))
                    .await;

                if let Some(ref client) = state.k8s_client {
                    let exists = k8s::check_agent_configmap_exists(
                        client,
                        &state.namespace,
                        &pending.agent_name,
                    )
                    .await?;
                    if !exists {
                        let err_msg = format!(
                            "⚠️ *Chef Amaraki* : Déploiement refusé. ConfigMap de l'agent `{}` introuvable dans `{}`.",
                            pending.agent_name, state.namespace
                        );
                        state
                            .slack_notifier
                            .post_message(&event.channel, &err_msg, Some(&thread_id))
                            .await;
                        state.metrics.record_request(false);
                        return Ok(());
                    }

                    match k8s::spawn_agent_job(
                        client,
                        &state.namespace,
                        &state.agent_runner_image,
                        &pending.agent_name,
                        &pending.full_prompt,
                        &event.channel,
                        &thread_id,
                        &state.metrics,
                    )
                    .await
                    {
                        Ok(job_id) => {
                            let success_msg = format!(
                                "✅ *Job K8s créé* : `{}`\n• *Agent* : *{}*\n• *Namespace* : `{}`\nL'agent exécute sa mission...",
                                job_id, pending.agent_name, state.namespace
                            );
                            state
                                .slack_notifier
                                .post_message(&event.channel, &success_msg, Some(&thread_id))
                                .await;
                        }
                        Err(e) => {
                            let err_msg = format!(
                                "💥 *Erreur de déploiement Job K8s* pour `{}` : {}",
                                pending.agent_name, e
                            );
                            state
                                .slack_notifier
                                .post_message(&event.channel, &err_msg, Some(&thread_id))
                                .await;
                            return Err(e.into());
                        }
                    }
                } else {
                    let sim_id = format!(
                        "{}-sim-{}",
                        pending.agent_name,
                        &uuid::Uuid::new_v4().to_string()[..8]
                    );
                    let sim_msg = format!(
                        "🤖 *Chef Amaraki* : [Mode autonome] Simulation de mission lancée pour `{}` (ID: `{}`).",
                        pending.agent_name, sim_id
                    );
                    state
                        .slack_notifier
                        .post_message(&event.channel, &sim_msg, Some(&thread_id))
                        .await;
                    state
                        .metrics
                        .record_agent_spawn(
                            &sim_id,
                            &pending.agent_name,
                            "opencode/free-default-model",
                            &event.channel,
                            &pending.full_prompt,
                        )
                        .await;
                }
                return Ok(());
            }
            UserValidationIntent::No => {
                // "Non : Demande de clarification ou correction par l'utilisateur."
                let clarify_msg = "Pouvez-vous préciser ce que vous souhaitez corriger ou modifier ? (Décrivez vos ajustements ou répondez Stop pour annuler)".to_string();
                state
                    .slack_notifier
                    .post_message(&event.channel, &clarify_msg, Some(&thread_id))
                    .await;

                let mut updated = pending;
                updated.awaiting_clarification = true;
                state.gatekeeper.set_pending(session_key, updated).await;
                return Ok(());
            }
            UserValidationIntent::Stop => {
                // "Stop : Annulation immédiate et clôture de la tâche en cours."
                state.gatekeeper.remove_pending(&session_key).await;
                let stop_msg = "🛑 *Chef Amaraki* : Action annulée et tâche clôturée.".to_string();
                state
                    .slack_notifier
                    .post_message(&event.channel, &stop_msg, Some(&thread_id))
                    .await;
                return Ok(());
            }
            UserValidationIntent::NewInstruction(clarification) => {
                if pending.awaiting_clarification {
                    // Intègre la correction et reformule la demande de validation
                    let combined_prompt =
                        format!("{} (Précision: {})", pending.full_prompt, clarification);
                    let (agent, summary) = GatekeeperStore::analyze_intent(&combined_prompt);

                    let validation_msg = format!(
                        "J'ai compris que vous souhaitez {}. Êtes-vous d'accord ? (Répondez par Oui, Non ou Stop)",
                        summary
                    );

                    let new_pending = PendingValidation {
                        channel: event.channel.clone(),
                        thread_ts: thread_id.clone(),
                        user: event.user.clone(),
                        agent_name: agent,
                        action_summary: summary,
                        full_prompt: combined_prompt,
                        awaiting_clarification: false,
                        created_at: chrono::Utc::now(),
                    };
                    state.gatekeeper.set_pending(session_key, new_pending).await;
                    state
                        .slack_notifier
                        .post_message(&event.channel, &validation_msg, Some(&thread_id))
                        .await;
                    return Ok(());
                }
            }
        }
    }

    // 3. Accusé de réception immédiat dans le fil de discussion Slack
    let ack_msg = "🫡 *Chef Aramaki (Section 9)* : Bien reçu. Requête en cours d'analyse...";
    state
        .slack_notifier
        .post_message(&event.channel, ack_msg, Some(&thread_id))
        .await;
    info!(
        "[SLACK] 🫡 Accusé de réception envoyé dans le thread {:?} (canal: {})",
        thread_id, event.channel
    );

    // Analyse d'intention et Demande formelle de validation
    let (agent_name, action_summary) = GatekeeperStore::analyze_intent(&instruction_text);

    // Formule exacte spécifiée :
    // "J'ai compris que vous souhaitez [résumé de l'action]. Êtes-vous d'accord ? (Répondez par Oui, Non ou Stop)"
    let validation_question = format!(
        "J'ai compris que vous souhaitez {}. Êtes-vous d'accord ? (Répondez par Oui, Non ou Stop)",
        action_summary
    );

    let pending = PendingValidation {
        channel: event.channel.clone(),
        thread_ts: thread_id.clone(),
        user: event.user.clone(),
        agent_name,
        action_summary,
        full_prompt: instruction_text,
        awaiting_clarification: false,
        created_at: chrono::Utc::now(),
    };

    state.gatekeeper.set_pending(session_key, pending).await;
    state
        .slack_notifier
        .post_message(&event.channel, &validation_question, Some(&thread_id))
        .await;

    Ok(())
}
