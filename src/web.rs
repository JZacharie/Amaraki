use crate::auth::{AuthConfig, LoginPayload, LoginResponse};
use crate::metrics::MetricsStore;
use crate::slack::SlackNotifier;
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use std::sync::Arc;

const LOGO_PNG: &[u8] = include_bytes!("../assets/logo.png");

#[derive(Clone)]
pub struct WebState {
    pub metrics: Arc<MetricsStore>,
    pub auth: Arc<AuthConfig>,
    pub namespace: String,
    pub k8s_client: Option<kube::Client>,
    pub agent_runner_image: String,
    pub slack_notifier: Arc<SlackNotifier>,
}

#[derive(Deserialize)]
pub struct AgentCallbackPayload {
    pub job_id: String,
    pub success: bool,
    pub duration_secs: Option<f64>,
    pub tool_calls: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct TestTriggerPayload {
    pub agent_name: String,
    pub prompt: String,
    pub channel: Option<String>,
}

#[derive(Deserialize)]
pub struct SendSlackMessagePayload {
    pub channel: Option<String>,
    pub text: String,
}

// Handler for the logo PNG
pub async fn logo_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        LOGO_PNG,
    )
}

// Handler for the main Dashboard HTML page
pub async fn dashboard_html_handler() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

// Handler for the Login HTML page
pub async fn login_html_handler() -> Html<&'static str> {
    Html(LOGIN_HTML)
}

// Dashboard statistics API
pub async fn dashboard_stats_handler(State(state): State<Arc<WebState>>) -> impl IntoResponse {
    let summary = state.metrics.get_dashboard_summary(&state.namespace).await;
    Json(summary)
}

// Agent configuration JSON export API
pub async fn agent_config_handler(
    State(state): State<Arc<WebState>>,
    Path(agent_name): Path<String>,
) -> impl IntoResponse {
    if let Some(agent) = state.metrics.get_agent(&agent_name).await {
        let config = serde_json::json!({
            "name": agent.name,
            "description": agent.description,
            "model": agent.model,
            "system_prompt": agent.system_prompt,
            "mcp_servers": agent.mcp_servers,
            "max_iterations": agent.max_iterations.unwrap_or(10),
            "env": agent.env,
            "tools_exposed": agent.tools_exposed,
            "stats": {
                "total_runs": agent.total_runs,
                "success_count": agent.success_count,
                "failure_count": agent.failure_count,
                "running_count": agent.running_count,
                "total_duration_secs": agent.total_duration_secs,
                "last_run": agent.last_run
            }
        });
        (StatusCode::OK, Json(config)).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Agent non trouvé",
                "agent": agent_name
            })),
        )
            .into_response()
    }
}

// Prometheus metrics endpoint
pub async fn prometheus_metrics_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
) -> Response {
    // If anonymous scraping is disallowed, enforce auth
    if !state.auth.allow_anonymous_metrics {
        let is_authed = if let Some(token) = crate::auth::extract_session_cookie(&headers) {
            state.auth.validate_token(&token).await.is_some()
        } else if let Some(token) = crate::auth::extract_bearer_token(&headers) {
            state.auth.validate_api_key(&token) || state.auth.validate_token(&token).await.is_some()
        } else if let Some((user, pass)) = crate::auth::extract_basic_auth(&headers) {
            state.auth.validate_credentials(&user, &pass)
        } else {
            false
        };

        if !is_authed {
            let mut resp = (
                StatusCode::UNAUTHORIZED,
                "Authentification requise pour le scraping des métriques Prometheus\n",
            )
                .into_response();
            resp.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                header::HeaderValue::from_static("Basic realm=\"Aramaki Metrics\""),
            );
            return resp;
        }
    }

    let text = state.metrics.to_prometheus_text(&state.namespace).await;
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        text,
    )
        .into_response()
}

// OpenTelemetry JSON metrics endpoint
pub async fn opentelemetry_metrics_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
) -> Response {
    if !state.auth.allow_anonymous_metrics {
        let is_authed = if let Some(token) = crate::auth::extract_session_cookie(&headers) {
            state.auth.validate_token(&token).await.is_some()
        } else if let Some(token) = crate::auth::extract_bearer_token(&headers) {
            state.auth.validate_api_key(&token) || state.auth.validate_token(&token).await.is_some()
        } else if let Some((user, pass)) = crate::auth::extract_basic_auth(&headers) {
            state.auth.validate_credentials(&user, &pass)
        } else {
            false
        };

        if !is_authed {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "Authentification requise" })),
            )
                .into_response();
        }
    }

    let otel_json = state.metrics.to_opentelemetry_json(&state.namespace).await;
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        Json(otel_json),
    )
        .into_response()
}

// Auth Login handler
pub async fn login_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Json(payload): Json<LoginPayload>,
) -> impl IntoResponse {
    let client_ip = crate::auth::extract_client_ip(&headers);
    if state
        .auth
        .validate_credentials(&payload.username, &payload.password)
    {
        tracing::info!(
            "[AUTH] ✅ Connexion réussie pour l'utilisateur '{}' | ip: {}",
            payload.username,
            client_ip
        );
        let session_token = state.auth.create_session(&payload.username).await;
        let cookie_val = format!(
            "aramaki_session={}; HttpOnly; SameSite=Lax; Path=/; Max-Age=86400",
            session_token
        );

        let mut response = (
            StatusCode::OK,
            Json(LoginResponse {
                status: "success".to_string(),
                username: payload.username,
                message: "Authentification réussie".to_string(),
            }),
        )
            .into_response();

        if let Ok(cookie_header) = header::HeaderValue::from_str(&cookie_val) {
            response
                .headers_mut()
                .insert(header::SET_COOKIE, cookie_header);
        }

        response
    } else {
        tracing::warn!(
            "[AUTH] ❌ Échec d'authentification pour l'utilisateur '{}' | ip: {}",
            payload.username,
            client_ip
        );
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "status": "error",
                "message": "Identifiants invalides"
            })),
        )
            .into_response()
    }
}

// Auth Logout handler
pub async fn logout_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let client_ip = crate::auth::extract_client_ip(&headers);
    if let Some(token) = crate::auth::extract_session_cookie(&headers) {
        state.auth.invalidate_session(&token).await;
    }
    tracing::info!("[AUTH] 🚪 Déconnexion utilisateur | ip: {}", client_ip);

    let cookie_clear = "aramaki_session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0";
    let mut response = (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "logged_out" })),
    )
        .into_response();
    if let Ok(cookie_header) = header::HeaderValue::from_str(cookie_clear) {
        response
            .headers_mut()
            .insert(header::SET_COOKIE, cookie_header);
    }
    response
}

// Auth Status check
pub async fn auth_me_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = crate::auth::extract_session_cookie(&headers) {
        if let Some(user) = state.auth.validate_token(&token).await {
            return Json(serde_json::json!({
                "authenticated": true,
                "username": user
            }));
        }
    }
    if let Some((user, pass)) = crate::auth::extract_basic_auth(&headers) {
        if state.auth.validate_credentials(&user, &pass) {
            return Json(serde_json::json!({
                "authenticated": true,
                "username": user
            }));
        }
    }

    Json(serde_json::json!({
        "authenticated": false
    }))
}

// Agent reporting callback
pub async fn agent_callback_handler(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<AgentCallbackPayload>,
) -> impl IntoResponse {
    tracing::info!(
        "[ACTION] 📡 Callback reçu pour le job '{}': succès={}, durée={:?}s, outils={:?}",
        payload.job_id,
        payload.success,
        payload.duration_secs,
        payload.tool_calls
    );

    state
        .metrics
        .record_agent_completion(&payload.job_id, payload.success, payload.duration_secs)
        .await;

    if let Some(tools) = payload.tool_calls {
        for tool in tools {
            state.metrics.record_tool_invocation(&tool).await;
        }
    }

    // Notification Slack du rapport de mission
    let emoji = if payload.success { "✅" } else { "❌" };
    let status_str = if payload.success { "SUCCÈS" } else { "ÉCHEC" };
    let duration_str = payload
        .duration_secs
        .map(|d| format!("{:.1}s", d))
        .unwrap_or_else(|| "N/A".to_string());
    let slack_msg = format!(
        "{} *Chef Aramaki (Section 9)* : Rapport d'intervention pour `{}`\n• *Statut* : {}\n• *Durée* : {}",
        emoji, payload.job_id, status_str, duration_str
    );
    state
        .slack_notifier
        .post_message("#ai", &slack_msg, None)
        .await;

    Json(serde_json::json!({ "status": "recorded" }))
}

// Test trigger handler to spawn an agent on demand
pub async fn test_trigger_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Json(payload): Json<TestTriggerPayload>,
) -> impl IntoResponse {
    let client_ip = crate::auth::extract_client_ip(&headers);
    state.metrics.record_request(true);

    let channel = payload.channel.unwrap_or_else(|| "#ai".to_string());
    let prompt = payload.prompt;
    let agent_name = payload.agent_name;

    tracing::info!(
        "[ACTION] 🧪 Déclencheur manuel: Lancement de l'agent '{}' depuis le Dashboard (canal: '{}', prompt: \"{}\") | ip: {}",
        agent_name,
        channel,
        prompt,
        client_ip
    );

    if let Some(ref client) = state.k8s_client {
        match crate::k8s::spawn_agent_job(
            client,
            &state.namespace,
            &state.agent_runner_image,
            &agent_name,
            &prompt,
            &channel,
            "dashboard-trigger",
            &state.metrics,
        )
        .await
        {
            Ok(job_id) => {
                let slack_msg = format!(
                    "🫡 *Chef Aramaki (Section 9)* : Lancement de l'agent *{}* sur `{}`.\n• *Job K8s* : `{}`\n• *Mission* : \"{}\"",
                    agent_name, channel, job_id, prompt
                );
                state
                    .slack_notifier
                    .post_message(&channel, &slack_msg, None)
                    .await;

                (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "status": "spawned",
                        "job_id": job_id,
                        "agent": agent_name
                    })),
                )
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "status": "error",
                    "error": e.to_string()
                })),
            ),
        }
    } else {
        // Standalone simulated mode
        let sim_id = format!(
            "{}-sim-{}",
            agent_name,
            &uuid::Uuid::new_v4().to_string()[..8]
        );
        let slack_msg = format!(
            "🫡 *Chef Aramaki (Section 9)* : [Simulation] Agent *{}* prêt sur `{}`\n• *Mission* : \"{}\"",
            agent_name, channel, prompt
        );
        state
            .slack_notifier
            .post_message(&channel, &slack_msg, None)
            .await;
        state
            .metrics
            .record_agent_spawn(
                &sim_id,
                &agent_name,
                "opencode/free-default-model",
                &channel,
                &prompt,
            )
            .await;

        // Simulate asynchronous completion after 3 seconds
        let metrics_clone = state.metrics.clone();
        let sim_id_clone = sim_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            metrics_clone
                .record_agent_completion(&sim_id_clone, true, Some(3.2))
                .await;
            metrics_clone.record_tool_invocation("filesystem").await;
        });

        (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "simulated_spawn",
                "job_id": sim_id,
                "message": "Job simulé avec succès en mode autonome"
            })),
        )
    }
}

// Endpoint to post a custom message directly to Slack channel as Chef Aramaki
pub async fn slack_send_handler(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<SendSlackMessagePayload>,
) -> impl IntoResponse {
    let channel = payload.channel.unwrap_or_else(|| "#ai".to_string());
    let sent = state
        .slack_notifier
        .post_message(&channel, &payload.text, None)
        .await;

    Json(serde_json::json!({
        "status": if sent { "sent" } else { "failed" },
        "channel": channel,
        "configured": state.slack_notifier.is_configured()
    }))
}

// Embedded Login HTML
const LOGIN_HTML: &str = r#"<!DOCTYPE html>
<html lang="fr">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Aramaki // Section 9 - Authentification</title>
  <link rel="icon" type="image/png" href="/logo.png">
  <style>
    :root {
      --bg: #090d16;
      --card-bg: rgba(18, 24, 38, 0.85);
      --border: rgba(0, 242, 254, 0.2);
      --accent-cyan: #00f2fe;
      --accent-blue: #4facfe;
      --text: #e2e8f0;
      --text-muted: #94a3b8;
      --danger: #f43f5e;
    }
    * { box-sizing: border-box; margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif; }
    body {
      background: radial-gradient(circle at 50% 20%, #152238 0%, var(--bg) 80%);
      color: var(--text);
      min-height: 100vh;
      display: flex;
      align-items: center;
      justify-content: center;
      padding: 1.5rem;
    }
    .login-card {
      background: var(--card-bg);
      backdrop-filter: blur(16px);
      -webkit-backdrop-filter: blur(16px);
      border: 1px solid var(--border);
      border-radius: 16px;
      padding: 2.5rem;
      width: 100%;
      max-width: 420px;
      box-shadow: 0 20px 50px rgba(0,0,0,0.6), 0 0 30px rgba(0, 242, 254, 0.1);
      position: relative;
      overflow: hidden;
    }
    .login-card::before {
      content: '';
      position: absolute;
      top: 0; left: 0; right: 0; height: 3px;
      background: linear-gradient(90deg, var(--accent-cyan), var(--accent-blue));
    }
    .badge {
      display: inline-block;
      font-size: 0.72rem;
      letter-spacing: 0.1em;
      text-transform: uppercase;
      padding: 0.25rem 0.6rem;
      border-radius: 9999px;
      background: rgba(0, 242, 254, 0.1);
      color: var(--accent-cyan);
      border: 1px solid rgba(0, 242, 254, 0.3);
      margin-bottom: 1rem;
    }
    h1 {
      font-size: 1.6rem;
      font-weight: 700;
      letter-spacing: -0.02em;
      margin-bottom: 0.5rem;
      color: #fff;
    }
    p.subtitle {
      font-size: 0.88rem;
      color: var(--text-muted);
      margin-bottom: 2rem;
    }
    .form-group {
      margin-bottom: 1.25rem;
    }
    label {
      display: block;
      font-size: 0.8rem;
      text-transform: uppercase;
      letter-spacing: 0.05em;
      color: var(--text-muted);
      margin-bottom: 0.4rem;
    }
    input[type="text"], input[type="password"] {
      width: 100%;
      padding: 0.8rem 1rem;
      background: rgba(10, 15, 25, 0.7);
      border: 1px solid rgba(255, 255, 255, 0.1);
      border-radius: 8px;
      color: #fff;
      font-size: 0.95rem;
      transition: all 0.2s;
    }
    input[type="text"]:focus, input[type="password"]:focus {
      outline: none;
      border-color: var(--accent-cyan);
      box-shadow: 0 0 12px rgba(0, 242, 254, 0.25);
    }
    button {
      width: 100%;
      padding: 0.85rem;
      background: linear-gradient(135deg, var(--accent-cyan), var(--accent-blue));
      color: #090d16;
      font-size: 0.95rem;
      font-weight: 600;
      border: none;
      border-radius: 8px;
      cursor: pointer;
      margin-top: 1rem;
      transition: transform 0.15s, box-shadow 0.15s;
    }
    button:hover {
      box-shadow: 0 0 20px rgba(0, 242, 254, 0.4);
      transform: translateY(-1px);
    }
    .error-msg {
      margin-top: 1rem;
      padding: 0.75rem;
      background: rgba(244, 63, 94, 0.15);
      border: 1px solid var(--danger);
      border-radius: 8px;
      color: var(--danger);
      font-size: 0.85rem;
      display: none;
    }
    .security-note {
      margin-top: 1.75rem;
      text-align: center;
      font-size: 0.75rem;
      color: var(--text-muted);
      border-top: 1px solid rgba(255, 255, 255, 0.06);
      padding-top: 1rem;
    }
  </style>
</head>
<body>
  <div class="login-card">
    <div style="display:flex; justify-content:center; margin-bottom:1.5rem;">
      <img src="/logo.png" alt="Chef Aramaki" style="width:105px; height:105px; border-radius:50%; border:2px solid var(--accent-cyan); box-shadow:0 0 25px rgba(0,242,254,0.4); object-fit:cover;">
    </div>
    <span class="badge">Section 9 // Security Gateway</span>
    <h1>Aramaki Orchestrator</h1>
    <p class="subtitle">Connexion au centre de contrôle des agents</p>

    <form id="loginForm">
      <div class="form-group">
        <label for="username">Identifiant Agent / Admin</label>
        <input type="text" id="username" name="username" required autocomplete="username" placeholder="admin">
      </div>
      <div class="form-group">
        <label for="password">Clé de sécurité / Mot de passe</label>
        <input type="password" id="password" name="password" required autocomplete="current-password" placeholder="••••••••••••">
      </div>
      <button type="submit" id="submitBtn">Accéder au Dashboard</button>
      <div id="errorMsg" class="error-msg"></div>
    </form>

    <div class="security-note">
      Accès sécurisé réservé aux superviseurs Section 9.
    </div>
  </div>

  <script>
    const form = document.getElementById('loginForm');
    const errorDiv = document.getElementById('errorMsg');
    const submitBtn = document.getElementById('submitBtn');

    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      errorDiv.style.display = 'none';
      submitBtn.disabled = true;
      submitBtn.textContent = 'Authentification...';

      const username = document.getElementById('username').value;
      const password = document.getElementById('password').value;

      try {
        const resp = await fetch('/api/auth/login', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ username, password })
        });
        const data = await resp.json();
        if (resp.ok && data.status === 'success') {
          window.location.href = '/';
        } else {
          errorDiv.textContent = data.message || 'Échec de la connexion';
          errorDiv.style.display = 'block';
        }
      } catch (err) {
        errorDiv.textContent = 'Erreur de communication avec le serveur';
        errorDiv.style.display = 'block';
      } finally {
        submitBtn.disabled = false;
        submitBtn.textContent = 'Accéder au Dashboard';
      }
    });
  </script>
</body>
</html>
"#;

// Embedded Main Dashboard HTML
const DASHBOARD_HTML: &str = r#"<!DOCTYPE html>
<html lang="fr">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Aramaki // Section 9 Agent Orchestrator & Observability</title>
  <link rel="icon" type="image/png" href="/logo.png">
  <style>
    :root {
      --bg: #070a11;
      --card-bg: rgba(15, 23, 42, 0.75);
      --card-border: rgba(30, 41, 59, 0.8);
      --accent-cyan: #00f2fe;
      --accent-blue: #4facfe;
      --accent-purple: #8b5cf6;
      --emerald: #10b981;
      --amber: #f59e0b;
      --ruby: #f43f5e;
      --text: #f8fafc;
      --text-muted: #94a3b8;
      --font-mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    }
    * { box-sizing: border-box; margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, Cantarell, sans-serif; }
    body {
      background-color: var(--bg);
      background-image: 
        radial-gradient(circle at 10% 20%, rgba(0, 242, 254, 0.05) 0%, transparent 40%),
        radial-gradient(circle at 90% 80%, rgba(139, 92, 246, 0.05) 0%, transparent 40%);
      color: var(--text);
      min-height: 100vh;
      display: flex;
      flex-direction: column;
    }

    /* Header */
    header {
      background: rgba(10, 15, 26, 0.85);
      backdrop-filter: blur(12px);
      -webkit-backdrop-filter: blur(12px);
      border-bottom: 1px solid rgba(255, 255, 255, 0.08);
      padding: 1rem 2rem;
      display: flex;
      align-items: center;
      justify-content: space-between;
      position: sticky;
      top: 0;
      z-index: 100;
    }
    .brand {
      display: flex;
      align-items: center;
      gap: 1rem;
    }
    .logo-badge {
      width: 40px;
      height: 40px;
      border-radius: 10px;
      background: linear-gradient(135deg, var(--accent-cyan), var(--accent-blue));
      display: flex;
      align-items: center;
      justify-content: center;
      color: #070a11;
      font-weight: 900;
      font-size: 1.2rem;
      box-shadow: 0 0 15px rgba(0, 242, 254, 0.3);
    }
    .brand h1 {
      font-size: 1.25rem;
      font-weight: 700;
      letter-spacing: -0.02em;
    }
    .brand span.sub {
      font-size: 0.75rem;
      text-transform: uppercase;
      letter-spacing: 0.1em;
      color: var(--accent-cyan);
      display: block;
    }
    .header-actions {
      display: flex;
      align-items: center;
      gap: 1rem;
    }
    .badge-status {
      display: flex;
      align-items: center;
      gap: 0.5rem;
      padding: 0.35rem 0.85rem;
      border-radius: 9999px;
      font-size: 0.78rem;
      font-weight: 600;
      background: rgba(16, 185, 129, 0.1);
      color: var(--emerald);
      border: 1px solid rgba(16, 185, 129, 0.3);
    }
    .pulse-dot {
      width: 8px;
      height: 8px;
      border-radius: 50%;
      background: var(--emerald);
      box-shadow: 0 0 8px var(--emerald);
      animation: pulse 2s infinite;
    }
    @keyframes pulse {
      0% { transform: scale(0.95); opacity: 0.8; }
      50% { transform: scale(1.2); opacity: 1; }
      100% { transform: scale(0.95); opacity: 0.8; }
    }
    .btn-action {
      background: rgba(255, 255, 255, 0.05);
      border: 1px solid rgba(255, 255, 255, 0.1);
      color: var(--text);
      padding: 0.45rem 0.9rem;
      border-radius: 6px;
      font-size: 0.8rem;
      cursor: pointer;
      display: inline-flex;
      align-items: center;
      gap: 0.4rem;
      text-decoration: none;
      transition: all 0.2s;
    }
    .btn-action:hover {
      background: rgba(255, 255, 255, 0.12);
      border-color: rgba(255, 255, 255, 0.2);
    }
    .btn-logout {
      border-color: rgba(244, 63, 94, 0.3);
      color: var(--ruby);
    }
    .btn-logout:hover {
      background: rgba(244, 63, 94, 0.15);
    }

    /* Main Container */
    main {
      flex: 1;
      padding: 2rem;
      max-width: 1440px;
      margin: 0 auto;
      width: 100%;
    }

    /* KPI Grid */
    .kpi-grid {
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
      gap: 1.25rem;
      margin-bottom: 2rem;
    }
    .kpi-card {
      background: var(--card-bg);
      border: 1px solid var(--card-border);
      border-radius: 12px;
      padding: 1.25rem;
      backdrop-filter: blur(8px);
      position: relative;
      overflow: hidden;
      display: flex;
      flex-direction: column;
      justify-content: space-between;
      transition: transform 0.2s, border-color 0.2s;
    }
    .kpi-card:hover {
      transform: translateY(-2px);
      border-color: rgba(0, 242, 254, 0.3);
    }
    .kpi-title {
      font-size: 0.78rem;
      text-transform: uppercase;
      letter-spacing: 0.05em;
      color: var(--text-muted);
      margin-bottom: 0.5rem;
      display: flex;
      align-items: center;
      justify-content: space-between;
    }
    .kpi-value {
      font-size: 2rem;
      font-weight: 700;
      letter-spacing: -0.03em;
      color: #fff;
    }
    .kpi-sub {
      font-size: 0.78rem;
      color: var(--text-muted);
      margin-top: 0.4rem;
    }
    .gauge-container {
      display: flex;
      align-items: center;
      gap: 1rem;
    }
    .radial-gauge {
      width: 64px;
      height: 64px;
      transform: rotate(-90deg);
    }
    .radial-gauge circle {
      fill: none;
      stroke-width: 6;
      stroke-linecap: round;
    }
    .gauge-bg {
      stroke: rgba(255, 255, 255, 0.1);
    }
    .gauge-fill {
      stroke: var(--emerald);
      transition: stroke-dashoffset 0.8s ease;
    }

    /* Content Layout */
    .dashboard-layout {
      display: grid;
      grid-template-columns: 2fr 1fr;
      gap: 1.5rem;
      margin-bottom: 2rem;
    }
    @media (max-width: 1024px) {
      .dashboard-layout {
        grid-template-columns: 1fr;
      }
    }

    .panel {
      background: var(--card-bg);
      border: 1px solid var(--card-border);
      border-radius: 12px;
      padding: 1.5rem;
      backdrop-filter: blur(8px);
      margin-bottom: 1.5rem;
    }
    .panel-header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      margin-bottom: 1.25rem;
      padding-bottom: 0.75rem;
      border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    }
    .panel-title {
      font-size: 1.05rem;
      font-weight: 600;
      color: #fff;
      display: flex;
      align-items: center;
      gap: 0.5rem;
    }

    /* Tables */
    .table-container {
      overflow-x: auto;
    }
    table {
      width: 100%;
      border-collapse: collapse;
      text-align: left;
      font-size: 0.85rem;
    }
    th {
      padding: 0.75rem 1rem;
      font-weight: 600;
      color: var(--text-muted);
      border-bottom: 1px solid rgba(255, 255, 255, 0.08);
      text-transform: uppercase;
      font-size: 0.72rem;
      letter-spacing: 0.05em;
    }
    td {
      padding: 0.85rem 1rem;
      border-bottom: 1px solid rgba(255, 255, 255, 0.04);
      color: var(--text);
    }
    tr:hover td {
      background: rgba(255, 255, 255, 0.02);
    }
    .mono {
      font-family: var(--font-mono);
      font-size: 0.82rem;
    }
    .badge-pill {
      display: inline-block;
      padding: 0.2rem 0.5rem;
      border-radius: 4px;
      font-size: 0.72rem;
      font-weight: 600;
    }
    .badge-running { background: rgba(0, 242, 254, 0.15); color: var(--accent-cyan); border: 1px solid rgba(0, 242, 254, 0.3); }
    .badge-succeeded { background: rgba(16, 185, 129, 0.15); color: var(--emerald); border: 1px solid rgba(16, 185, 129, 0.3); }
    .badge-failed { background: rgba(244, 63, 94, 0.15); color: var(--ruby); border: 1px solid rgba(244, 63, 94, 0.3); }

    .tool-tag {
      display: inline-block;
      background: rgba(139, 92, 246, 0.15);
      color: #c4b5fd;
      border: 1px solid rgba(139, 92, 246, 0.3);
      padding: 0.15rem 0.45rem;
      border-radius: 4px;
      font-size: 0.7rem;
      margin-right: 0.3rem;
      margin-bottom: 0.2rem;
    }

    /* Progress bars */
    .bar-item {
      margin-bottom: 1rem;
    }
    .bar-info {
      display: flex;
      justify-content: space-between;
      font-size: 0.82rem;
      margin-bottom: 0.35rem;
    }
    .bar-track {
      height: 8px;
      background: rgba(255, 255, 255, 0.06);
      border-radius: 4px;
      overflow: hidden;
    }
    .bar-progress {
      height: 100%;
      background: linear-gradient(90deg, var(--accent-cyan), var(--accent-blue));
      border-radius: 4px;
      transition: width 0.6s ease;
    }

    /* Code Snippet Box */
    .code-box {
      background: #030712;
      border: 1px solid rgba(255, 255, 255, 0.1);
      border-radius: 8px;
      padding: 0.85rem;
      font-family: var(--font-mono);
      font-size: 0.8rem;
      color: #38bdf8;
      display: flex;
      align-items: center;
      justify-content: space-between;
      overflow-x: auto;
      margin-top: 0.75rem;
    }
    .code-box button {
      background: rgba(255, 255, 255, 0.1);
      border: none;
      color: #fff;
      padding: 0.3rem 0.6rem;
      border-radius: 4px;
      cursor: pointer;
      font-size: 0.75rem;
    }
    .code-box button:hover {
      background: rgba(255, 255, 255, 0.2);
    }

    /* Footer */
    footer {
      background: rgba(10, 15, 26, 0.95);
      border-top: 1px solid rgba(255, 255, 255, 0.06);
      padding: 1rem 2rem;
      text-align: center;
      font-size: 0.75rem;
      color: var(--text-muted);
    }

    /* Agent Row Interactivity */
    .agent-row {
      cursor: pointer;
      transition: background-color 0.15s ease, transform 0.1s ease;
    }
    .agent-row:hover td {
      background: rgba(0, 242, 254, 0.07) !important;
    }

    /* Modal Agent Details & Export JSON */
    .modal-overlay {
      position: fixed;
      top: 0; left: 0; right: 0; bottom: 0;
      background: rgba(3, 7, 18, 0.85);
      backdrop-filter: blur(8px);
      -webkit-backdrop-filter: blur(8px);
      z-index: 1000;
      display: none;
      align-items: center;
      justify-content: center;
      padding: 1rem;
    }
    .modal-overlay.active {
      display: flex;
    }
    .modal-content {
      background: #0f172a;
      border: 1px solid rgba(0, 242, 254, 0.3);
      border-radius: 14px;
      width: 100%;
      max-width: 860px;
      max-height: 90vh;
      display: flex;
      flex-direction: column;
      box-shadow: 0 25px 50px -12px rgba(0, 0, 0, 0.8), 0 0 35px rgba(0, 242, 254, 0.15);
      overflow: hidden;
      animation: modalFadeIn 0.2s ease-out;
    }
    @keyframes modalFadeIn {
      from { opacity: 0; transform: scale(0.97); }
      to { opacity: 1; transform: scale(1); }
    }
    .modal-header {
      padding: 1.25rem 1.5rem;
      border-bottom: 1px solid rgba(255, 255, 255, 0.08);
      display: flex;
      align-items: center;
      justify-content: space-between;
      background: rgba(15, 23, 42, 0.95);
    }
    .modal-title-group {
      display: flex;
      align-items: center;
      gap: 0.75rem;
    }
    .modal-title {
      font-size: 1.2rem;
      font-weight: 700;
      color: #fff;
    }
    .modal-close {
      background: transparent;
      border: none;
      color: var(--text-muted);
      font-size: 1.4rem;
      cursor: pointer;
      padding: 0.25rem 0.5rem;
      border-radius: 6px;
      line-height: 1;
      transition: color 0.15s, background 0.15s;
    }
    .modal-close:hover {
      color: #fff;
      background: rgba(255, 255, 255, 0.1);
    }
    .modal-tabs {
      display: flex;
      gap: 0.5rem;
      padding: 0.75rem 1.5rem 0 1.5rem;
      border-bottom: 1px solid rgba(255, 255, 255, 0.08);
      background: rgba(10, 15, 26, 0.6);
    }
    .modal-tab {
      padding: 0.5rem 1rem;
      font-size: 0.85rem;
      font-weight: 600;
      color: var(--text-muted);
      background: transparent;
      border: none;
      border-bottom: 2px solid transparent;
      cursor: pointer;
      transition: all 0.15s ease;
    }
    .modal-tab:hover {
      color: #fff;
    }
    .modal-tab.active {
      color: var(--accent-cyan);
      border-bottom: 2px solid var(--accent-cyan);
    }
    .modal-body {
      padding: 1.5rem;
      overflow-y: auto;
      flex: 1;
    }
    .modal-section {
      display: none;
    }
    .modal-section.active {
      display: block;
    }
    .prompt-box {
      background: #030712;
      border: 1px solid rgba(255, 255, 255, 0.1);
      border-radius: 8px;
      padding: 1rem 1.25rem;
      font-family: var(--font-mono);
      font-size: 0.82rem;
      color: #e2e8f0;
      line-height: 1.6;
      white-space: pre-wrap;
      word-break: break-word;
      max-height: 420px;
      overflow-y: auto;
    }
    .json-code-box {
      background: #030712;
      border: 1px solid rgba(0, 242, 254, 0.2);
      border-radius: 8px;
      padding: 1rem 1.25rem;
      font-family: var(--font-mono);
      font-size: 0.8rem;
      color: #38bdf8;
      line-height: 1.5;
      white-space: pre-wrap;
      word-break: break-word;
      max-height: 420px;
      overflow-y: auto;
    }
    .modal-footer {
      padding: 1rem 1.5rem;
      border-top: 1px solid rgba(255, 255, 255, 0.08);
      background: rgba(15, 23, 42, 0.95);
      display: flex;
      align-items: center;
      justify-content: space-between;
      flex-wrap: wrap;
      gap: 0.75rem;
    }
    .modal-actions-left {
      display: flex;
      gap: 0.5rem;
      flex-wrap: wrap;
    }
    .btn-copy {
      background: rgba(0, 242, 254, 0.1);
      color: var(--accent-cyan);
      border: 1px solid rgba(0, 242, 254, 0.3);
      padding: 0.45rem 0.85rem;
      border-radius: 6px;
      font-size: 0.8rem;
      font-weight: 600;
      cursor: pointer;
      display: inline-flex;
      align-items: center;
      gap: 0.4rem;
      transition: all 0.15s;
    }
    .btn-copy:hover {
      background: rgba(0, 242, 254, 0.25);
      box-shadow: 0 0 10px rgba(0, 242, 254, 0.3);
    }
    .btn-export {
      background: linear-gradient(135deg, var(--accent-cyan), var(--accent-blue));
      color: #090d16;
      border: none;
      padding: 0.45rem 0.95rem;
      border-radius: 6px;
      font-size: 0.8rem;
      font-weight: 700;
      cursor: pointer;
      display: inline-flex;
      align-items: center;
      gap: 0.4rem;
      transition: all 0.15s;
    }
    .btn-export:hover {
      box-shadow: 0 0 15px rgba(0, 242, 254, 0.5);
      transform: translateY(-1px);
    }
    .btn-close-modal {
      background: rgba(255, 255, 255, 0.06);
      color: var(--text-muted);
      border: 1px solid rgba(255, 255, 255, 0.1);
      padding: 0.45rem 0.85rem;
      border-radius: 6px;
      font-size: 0.8rem;
      cursor: pointer;
    }
    .btn-close-modal:hover {
      color: #fff;
      background: rgba(255, 255, 255, 0.12);
    }
    .meta-grid {
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
      gap: 0.75rem;
      margin-bottom: 1.25rem;
    }
    .meta-card {
      background: rgba(255, 255, 255, 0.02);
      border: 1px solid rgba(255, 255, 255, 0.06);
      border-radius: 6px;
      padding: 0.75rem;
    }
    .meta-card .label {
      font-size: 0.7rem;
      text-transform: uppercase;
      letter-spacing: 0.05em;
      color: var(--text-muted);
      margin-bottom: 0.25rem;
    }
    .meta-card .value {
      font-size: 0.85rem;
      font-weight: 600;
      color: #fff;
    }
  </style>
</head>
<body>
  <header>
    <div class="brand">
      <img src="/logo.png" alt="Chef Aramaki" style="width:48px; height:48px; border-radius:50%; border:2px solid var(--accent-cyan); box-shadow:0 0 16px rgba(0,242,254,0.35); object-fit:cover; margin-right:0.85rem; flex-shrink:0;">
      <div>
        <span class="sub">Chief Section 9 // K8s AI Orchestrator</span>
        <h1>Aramaki Monitor & Metrics</h1>
      </div>
    </div>
    <div class="header-actions">
      <div id="statusBadge" class="badge-status">
        <span class="pulse-dot"></span>
        <span id="statusText">Connexion...</span>
      </div>
      <select id="refreshInterval" class="btn-action" style="background:#090d16; color:#fff;">
        <option value="5000">Rafraîchir: 5s</option>
        <option value="10000" selected>Rafraîchir: 10s</option>
        <option value="30000">Rafraîchir: 30s</option>
        <option value="0">Manuel</option>
      </select>
      <button id="refreshBtn" class="btn-action">Actualiser</button>
      <a href="/metrics" target="_blank" class="btn-action">📊 Prometheus</a>
      <a href="/api/otel/v1/metrics" target="_blank" class="btn-action">🌐 OpenTelemetry</a>
      <button id="logoutBtn" class="btn-action btn-logout">Déconnexion</button>
    </div>
  </header>

  <main>
    <!-- KPI Row -->
    <div class="kpi-grid">
      <div class="kpi-card">
        <div class="kpi-title">
          <span>Requêtes Globales</span>
          <span>⚡</span>
        </div>
        <div class="kpi-value" id="kpiRequestsTotal">0</div>
        <div class="kpi-sub">
          <span style="color:var(--emerald);" id="kpiRequestsSuccess">0 succès</span> · 
          <span style="color:var(--ruby);" id="kpiRequestsFailed">0 échecs</span>
        </div>
      </div>

      <div class="kpi-card">
        <div class="kpi-title">
          <span>Agents Lancés</span>
          <span>🤖</span>
        </div>
        <div class="kpi-value" id="kpiAgentsSpawned">0</div>
        <div class="kpi-sub">
          <span style="color:var(--accent-cyan);" id="kpiAgentsRunning">0 actifs</span> · 
          <span style="color:var(--emerald);" id="kpiAgentsCompleted">0 terminés</span>
        </div>
      </div>

      <div class="kpi-card">
        <div class="kpi-title">
          <span>Taux de Réussite</span>
          <span>🎯</span>
        </div>
        <div class="gauge-container">
          <svg class="radial-gauge" viewBox="0 0 36 36">
            <circle class="gauge-bg" cx="18" cy="18" r="15.9155"></circle>
            <circle id="gaugeFill" class="gauge-fill" cx="18" cy="18" r="15.9155" stroke-dasharray="100 100" stroke-dashoffset="0"></circle>
          </svg>
          <div>
            <div class="kpi-value" id="kpiSuccessRate">100%</div>
            <div class="kpi-sub">Exécutions d'agents</div>
          </div>
        </div>
      </div>

      <div class="kpi-card">
        <div class="kpi-title">
          <span>Uptime & Namespace</span>
          <span>⏱️</span>
        </div>
        <div class="kpi-value" style="font-size:1.45rem;" id="kpiUptime">--</div>
        <div class="kpi-sub" id="kpiNamespace">Namespace: aramaki</div>
      </div>
    </div>

    <!-- Main Layout -->
    <div class="dashboard-layout">
      <!-- Left Column: Agents Table and Jobs -->
      <div>
        <!-- Agents Panel -->
        <div class="panel">
          <div class="panel-header">
            <div class="panel-title">
              <span>👥 Agents Déployés & Statistiques</span>
            </div>
            <span class="mono" style="font-size:0.75rem; color:var(--text-muted);" id="agentsCount">0 agents configurés</span>
          </div>
          <div class="table-container">
            <table>
              <thead>
                <tr>
                  <th>Agent <span style="font-weight:normal; font-size:0.68rem; color:var(--accent-cyan); text-transform:none;">(cliquer pour inspecter & exporter)</span></th>
                  <th>Modèle Configuré</th>
                  <th>Outils Exposés (MCP)</th>
                  <th>Runs</th>
                  <th>Succès</th>
                  <th>Taux</th>
                  <th>Durée Moy.</th>
                </tr>
              </thead>
              <tbody id="agentsTableBody">
                <tr><td colspan="7" style="text-align:center; color:var(--text-muted);">Chargement des agents...</td></tr>
              </tbody>
            </table>
          </div>
        </div>

        <!-- Recent Executions Panel -->
        <div class="panel">
          <div class="panel-header">
            <div class="panel-title">
              <span>📜 Historique des Exécutions Récentes</span>
            </div>
            <span class="mono" style="font-size:0.75rem; color:var(--text-muted);" id="jobsCount">0 jobs</span>
          </div>
          <div class="table-container">
            <table>
              <thead>
                <tr>
                  <th>ID Job</th>
                  <th>Agent</th>
                  <th>Canal</th>
                  <th>Statut</th>
                  <th>Durée</th>
                  <th>Heure</th>
                </tr>
              </thead>
              <tbody id="executionsTableBody">
                <tr><td colspan="6" style="text-align:center; color:var(--text-muted);">Aucune exécution récente.</td></tr>
              </tbody>
            </table>
          </div>
        </div>
      </div>

      <!-- Right Column: Models & Tools & Prometheus Links -->
      <div>
        <!-- Models Panel -->
        <div class="panel">
          <div class="panel-header">
            <div class="panel-title">
              <span>🧠 Modèles d'IA Consommés</span>
            </div>
          </div>
          <div id="modelsContainer">
            <p style="color:var(--text-muted); font-size:0.85rem;">En attente de données...</p>
          </div>
        </div>

        <!-- Exposed Tools Panel -->
        <div class="panel">
          <div class="panel-header">
            <div class="panel-title">
              <span>🛠️ Outils (MCP) Détectés</span>
            </div>
          </div>
          <div id="toolsContainer">
            <p style="color:var(--text-muted); font-size:0.85rem;">Aucun outil détecté.</p>
          </div>
        </div>

        <!-- Observability Endpoints Panel -->
        <div class="panel">
          <div class="panel-header">
            <div class="panel-title">
              <span>📡 Endpoints Métriques</span>
            </div>
          </div>
          <p style="font-size:0.82rem; color:var(--text-muted); margin-bottom:0.75rem;">
            Exposition conforme aux standards Prometheus et OpenTelemetry (OTel) pour Grafana et OpenObserve.
          </p>

          <div style="font-size:0.78rem; color:#fff; margin-top:0.5rem;">Prometheus Scrape (/metrics):</div>
          <div class="code-box">
            <span>curl -s http://localhost:3000/metrics</span>
            <button onclick="copySnippet('curl -s http://localhost:3000/metrics')">Copier</button>
          </div>

          <div style="font-size:0.78rem; color:#fff; margin-top:0.75rem;">OpenTelemetry JSON (/api/otel/v1/metrics):</div>
          <div class="code-box">
            <span>curl -s http://localhost:3000/api/otel/v1/metrics</span>
            <button onclick="copySnippet('curl -s http://localhost:3000/api/otel/v1/metrics')">Copier</button>
          </div>
        </div>
      </div>
    </div>
  </main>

  <footer>
    Aramaki v0.1.0 // Section 9 Autonomous Orchestration & Dynamic Provisioning Cluster
  </footer>

  <!-- Modal Agent Details & Export JSON -->
  <div id="agentModal" class="modal-overlay" role="dialog" aria-modal="true" aria-labelledby="modalAgentName">
    <div class="modal-content">
      <div class="modal-header">
        <div class="modal-title-group">
          <span style="font-size:1.3rem;">🤖</span>
          <div>
            <h2 id="modalAgentName" class="modal-title">agent-name</h2>
            <div style="font-size:0.75rem; color:var(--text-muted);" id="modalAgentDesc">Description de l'agent</div>
          </div>
          <span id="modalModelBadge" class="badge-pill mono" style="background:rgba(56,189,248,0.15); color:#38bdf8; border:1px solid rgba(56,189,248,0.3); margin-left:0.5rem;">modèle</span>
        </div>
        <button id="modalCloseBtn" class="modal-close" aria-label="Fermer la modal">&times;</button>
      </div>

      <div class="modal-tabs">
        <button class="modal-tab active" data-tab="tab-prompt">📝 Instructions (Prompt Système)</button>
        <button class="modal-tab" data-tab="tab-json">📦 Configuration JSON (Export IDE)</button>
        <button class="modal-tab" data-tab="tab-params">⚙️ Paramètres & MCP</button>
      </div>

      <div class="modal-body">
        <!-- Tab 1: System Prompt / Instructions -->
        <div id="tab-prompt" class="modal-section active">
          <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:0.6rem;">
            <span style="font-size:0.8rem; color:var(--text-muted); font-weight:600;">Directives & Règles de comportement de l'agent :</span>
            <button id="btnCopyPrompt" class="btn-copy" style="padding:0.25rem 0.6rem; font-size:0.75rem;">📋 Copier les Instructions</button>
          </div>
          <div id="modalPromptContent" class="prompt-box">Chargement des instructions...</div>
        </div>

        <!-- Tab 2: Export JSON -->
        <div id="tab-json" class="modal-section">
          <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:0.6rem;">
            <span style="font-size:0.8rem; color:var(--text-muted); font-weight:600;">Spécification JSON complète (export / modification dans votre IDE) :</span>
            <div style="display:flex; gap:0.5rem;">
              <button id="btnCopyJsonTab" class="btn-copy" style="padding:0.25rem 0.6rem; font-size:0.75rem;">📋 Copier JSON</button>
              <button id="btnDownloadJsonTab" class="btn-export" style="padding:0.25rem 0.6rem; font-size:0.75rem;">📥 Télécharger .json</button>
            </div>
          </div>
          <pre id="modalJsonContent" class="json-code-box">{}</pre>
        </div>

        <!-- Tab 3: Parameters & Tools -->
        <div id="tab-params" class="modal-section">
          <div class="meta-grid">
            <div class="meta-card">
              <div class="label">Modèle LLM</div>
              <div class="value mono" id="metaModel">--</div>
            </div>
            <div class="meta-card">
              <div class="label">Itérations Max (Loop Guard)</div>
              <div class="value mono" id="metaMaxIterations">--</div>
            </div>
            <div class="meta-card">
              <div class="label">Runs / Exécutions</div>
              <div class="value mono" id="metaRuns">--</div>
            </div>
            <div class="meta-card">
              <div class="label">Taux de Réussite</div>
              <div class="value mono" id="metaSuccessRate">--</div>
            </div>
          </div>

          <div style="margin-bottom:1rem;">
            <div style="font-size:0.75rem; text-transform:uppercase; letter-spacing:0.05em; color:var(--text-muted); margin-bottom:0.4rem;">Serveurs & Outils MCP</div>
            <div id="metaMcpServers" style="background:#030712; border:1px solid rgba(255,255,255,0.08); border-radius:6px; padding:0.75rem; font-size:0.8rem;"></div>
          </div>

          <div>
            <div style="font-size:0.75rem; text-transform:uppercase; letter-spacing:0.05em; color:var(--text-muted); margin-bottom:0.4rem;">Variables d'environnement</div>
            <div id="metaEnvVars" class="mono" style="background:#030712; border:1px solid rgba(255,255,255,0.08); border-radius:6px; padding:0.75rem; font-size:0.75rem; color:#94a3b8; white-space:pre-wrap;"></div>
          </div>
        </div>
      </div>

      <div class="modal-footer">
        <div class="modal-actions-left">
          <button id="btnCopyInstructionsFooter" class="btn-copy">📋 Copier les Instructions</button>
          <button id="btnCopyJsonFooter" class="btn-copy">📋 Copier la Config JSON</button>
          <button id="btnDownloadJsonFooter" class="btn-export">📥 Télécharger JSON</button>
        </div>
        <button id="btnCloseModalFooter" class="btn-close-modal">Fermer</button>
      </div>
    </div>
  </div>

  <script>
    let refreshTimer = null;

    async function fetchStats() {
      try {
        const resp = await fetch('/api/dashboard/stats');
        if (resp.status === 401) {
          window.location.href = '/login';
          return;
        }
        if (!resp.ok) throw new Error('HTTP ' + resp.status);
        const data = await resp.json();
        renderDashboard(data);
      } catch (err) {
        console.error('Erreur récupération stats:', err);
        const statusText = document.getElementById('statusText');
        const statusBadge = document.getElementById('statusBadge');
        if (statusText && statusBadge) {
          statusText.textContent = 'Erreur synchronisation';
          statusBadge.style.color = 'var(--ruby)';
          statusBadge.style.borderColor = 'rgba(244, 63, 94, 0.3)';
          statusBadge.style.background = 'rgba(244, 63, 94, 0.1)';
        }
      }
    }

    function renderDashboard(data) {
      // 1. Status & Header
      const statusText = document.getElementById('statusText');
      const statusBadge = document.getElementById('statusBadge');
      if (data.status && data.status.k8s_connected) {
        statusText.textContent = 'K8s Connecté (' + data.status.namespace + ')';
        statusBadge.style.color = 'var(--emerald)';
        statusBadge.style.borderColor = 'rgba(16, 185, 129, 0.3)';
        statusBadge.style.background = 'rgba(16, 185, 129, 0.1)';
      } else {
        statusText.textContent = data.status.mode || 'Mode Autonome';
        statusBadge.style.color = 'var(--amber)';
        statusBadge.style.borderColor = 'rgba(245, 158, 11, 0.3)';
        statusBadge.style.background = 'rgba(245, 158, 11, 0.1)';
      }

      // 2. KPIs
      document.getElementById('kpiRequestsTotal').textContent = data.requests_total || 0;
      document.getElementById('kpiRequestsSuccess').textContent = (data.requests_success || 0) + ' succès';
      document.getElementById('kpiRequestsFailed').textContent = (data.requests_failed || 0) + ' échecs';

      document.getElementById('kpiAgentsSpawned').textContent = data.agents_spawned_total || 0;
      document.getElementById('kpiAgentsRunning').textContent = (data.agents_running || 0) + ' actifs';
      document.getElementById('kpiAgentsCompleted').textContent = (data.agents_succeeded || 0) + ' terminés';

      const successRate = Math.round(data.global_success_rate || 100);
      document.getElementById('kpiSuccessRate').textContent = successRate + '%';
      
      const gaugeFill = document.getElementById('gaugeFill');
      const offset = 100 - successRate;
      gaugeFill.setAttribute('stroke-dashoffset', offset);
      if (successRate >= 90) {
        gaugeFill.style.stroke = 'var(--emerald)';
      } else if (successRate >= 70) {
        gaugeFill.style.stroke = 'var(--amber)';
      } else {
        gaugeFill.style.stroke = 'var(--ruby)';
      }

      document.getElementById('kpiUptime').textContent = data.uptime_formatted || '--';
      document.getElementById('kpiNamespace').textContent = 'Namespace: ' + (data.status.namespace || 'aramaki');

      // 3. Agents Table
      renderAgentsTable(data.agents || []);

      // 4. Executions Table
      renderExecutionsTable(data.recent_executions || []);

      // 5. Models Breakdown
      renderModels(data.models || []);

      // 6. Tools Breakdown
      renderTools(data.tools || []);
    }

    function renderAgentsTable(agents) {
      const tbody = document.getElementById('agentsTableBody');
      tbody.replaceChildren();

      document.getElementById('agentsCount').textContent = agents.length + ' agents configurés';

      if (agents.length === 0) {
        const tr = document.createElement('tr');
        const td = document.createElement('td');
        td.colSpan = 7;
        td.style.textAlign = 'center';
        td.style.color = 'var(--text-muted)';
        td.textContent = 'Aucun agent configuré pour le moment.';
        tr.appendChild(td);
        tbody.appendChild(tr);
        return;
      }

      agents.forEach(agent => {
        const tr = document.createElement('tr');
        tr.className = 'agent-row';
        tr.title = 'Cliquez pour afficher les instructions et exporter la configuration JSON de ' + agent.name;
        tr.addEventListener('click', () => openAgentModal(agent));

        // Name
        const tdName = document.createElement('td');
        tdName.style.fontWeight = '600';
        
        const nameWrapper = document.createElement('div');
        nameWrapper.style.display = 'flex';
        nameWrapper.style.alignItems = 'center';
        nameWrapper.style.gap = '0.5rem';

        const nameSpan = document.createElement('span');
        nameSpan.textContent = agent.name;
        nameWrapper.appendChild(nameSpan);

        const inspectPill = document.createElement('span');
        inspectPill.className = 'badge-pill';
        inspectPill.style.background = 'rgba(0, 242, 254, 0.1)';
        inspectPill.style.color = 'var(--accent-cyan)';
        inspectPill.style.border = '1px solid rgba(0, 242, 254, 0.25)';
        inspectPill.style.fontSize = '0.68rem';
        inspectPill.style.cursor = 'pointer';
        inspectPill.textContent = 'Détails & JSON ↗';
        nameWrapper.appendChild(inspectPill);

        tdName.appendChild(nameWrapper);
        tr.appendChild(tdName);

        // Model
        const tdModel = document.createElement('td');
        tdModel.className = 'mono';
        tdModel.style.color = '#38bdf8';
        tdModel.textContent = agent.model;
        tr.appendChild(tdModel);

        // Tools
        const tdTools = document.createElement('td');
        if (agent.tools_exposed && agent.tools_exposed.length > 0) {
          agent.tools_exposed.forEach(tool => {
            const span = document.createElement('span');
            span.className = 'tool-tag';
            span.textContent = tool;
            tdTools.appendChild(span);
          });
        } else {
          tdTools.textContent = 'Aucun';
          tdTools.style.color = 'var(--text-muted)';
        }
        tr.appendChild(tdTools);

        // Runs
        const tdRuns = document.createElement('td');
        tdRuns.textContent = agent.total_runs;
        tr.appendChild(tdRuns);

        // Success / Failures
        const tdSuccess = document.createElement('td');
        tdSuccess.textContent = agent.success_count + ' / ' + agent.failure_count;
        tr.appendChild(tdSuccess);

        // Rate
        const tdRate = document.createElement('td');
        const completed = agent.success_count + agent.failure_count;
        const rate = completed === 0 ? 100 : Math.round((agent.success_count / completed) * 100);
        const rateBadge = document.createElement('span');
        rateBadge.className = 'badge-pill ' + (rate >= 80 ? 'badge-succeeded' : rate >= 50 ? 'badge-running' : 'badge-failed');
        rateBadge.textContent = rate + '%';
        tdRate.appendChild(rateBadge);
        tr.appendChild(tdRate);

        // Avg Duration
        const tdDur = document.createElement('td');
        const avg = completed === 0 ? 0 : (agent.total_duration_secs / completed).toFixed(1);
        tdDur.className = 'mono';
        tdDur.textContent = avg + 's';
        tr.appendChild(tdDur);

        tbody.appendChild(tr);
      });
    }

    function renderExecutionsTable(executions) {
      const tbody = document.getElementById('executionsTableBody');
      tbody.replaceChildren();

      document.getElementById('jobsCount').textContent = executions.length + ' jobs';

      if (executions.length === 0) {
        const tr = document.createElement('tr');
        const td = document.createElement('td');
        td.colSpan = 6;
        td.style.textAlign = 'center';
        td.style.color = 'var(--text-muted)';
        td.textContent = 'Aucun job enregistré.';
        tr.appendChild(td);
        tbody.appendChild(tr);
        return;
      }

      executions.forEach(exec => {
        const tr = document.createElement('tr');

        // ID
        const tdId = document.createElement('td');
        tdId.className = 'mono';
        tdId.style.color = '#94a3b8';
        tdId.textContent = exec.id;
        tr.appendChild(tdId);

        // Agent
        const tdAgent = document.createElement('td');
        tdAgent.style.fontWeight = '600';
        tdAgent.textContent = exec.agent_name;
        tr.appendChild(tdAgent);

        // Channel
        const tdChan = document.createElement('td');
        tdChan.className = 'mono';
        tdChan.textContent = exec.channel;
        tr.appendChild(tdChan);

        // Status
        const tdStatus = document.createElement('td');
        const statusSpan = document.createElement('span');
        statusSpan.className = 'badge-pill badge-' + exec.status;
        statusSpan.textContent = exec.status.toUpperCase();
        tdStatus.appendChild(statusSpan);
        tr.appendChild(tdStatus);

        // Duration
        const tdDur = document.createElement('td');
        tdDur.className = 'mono';
        tdDur.textContent = exec.duration_secs ? exec.duration_secs.toFixed(1) + 's' : 'en cours';
        tr.appendChild(tdDur);

        // Time
        const tdTime = document.createElement('td');
        tdTime.className = 'mono';
        tdTime.style.color = 'var(--text-muted)';
        const date = new Date(exec.started_at);
        tdTime.textContent = date.toLocaleTimeString();
        tr.appendChild(tdTime);

        tbody.appendChild(tr);
      });
    }

    function renderModels(models) {
      const container = document.getElementById('modelsContainer');
      container.replaceChildren();

      if (models.length === 0) {
        const p = document.createElement('p');
        p.style.color = 'var(--text-muted)';
        p.style.fontSize = '0.85rem';
        p.textContent = 'Aucun modèle consommé pour le moment.';
        container.appendChild(p);
        return;
      }

      const totalInvocations = models.reduce((acc, m) => acc + m.invocations, 0) || 1;

      models.forEach(model => {
        const div = document.createElement('div');
        div.className = 'bar-item';

        const info = document.createElement('div');
        info.className = 'bar-info';
        
        const titleSpan = document.createElement('span');
        titleSpan.className = 'mono';
        titleSpan.style.color = '#fff';
        titleSpan.textContent = model.model;

        const countSpan = document.createElement('span');
        countSpan.className = 'mono';
        countSpan.style.color = 'var(--accent-cyan)';
        countSpan.textContent = model.invocations + ' appel(s)';

        info.appendChild(titleSpan);
        info.appendChild(countSpan);

        const track = document.createElement('div');
        track.className = 'bar-track';
        const prog = document.createElement('div');
        prog.className = 'bar-progress';
        const pct = Math.round((model.invocations / totalInvocations) * 100);
        prog.style.width = pct + '%';
        track.appendChild(prog);

        div.appendChild(info);
        div.appendChild(track);
        container.appendChild(div);
      });
    }

    function renderTools(tools) {
      const container = document.getElementById('toolsContainer');
      container.replaceChildren();

      if (tools.length === 0) {
        const p = document.createElement('p');
        p.style.color = 'var(--text-muted)';
        p.style.fontSize = '0.85rem';
        p.textContent = 'Aucun outil MCP détecté.';
        container.appendChild(p);
        return;
      }

      tools.forEach(tool => {
        const div = document.createElement('div');
        div.style.padding = '0.65rem';
        div.style.marginBottom = '0.5rem';
        div.style.background = 'rgba(255,255,255,0.02)';
        div.style.border = '1px solid rgba(255,255,255,0.05)';
        div.style.borderRadius = '6px';
        div.style.display = 'flex';
        div.style.alignItems = 'center';
        div.style.justifyContent = 'space-between';

        const left = document.createElement('div');
        const name = document.createElement('div');
        name.style.fontWeight = '600';
        name.style.fontSize = '0.85rem';
        name.textContent = '🔧 ' + tool.tool_name;

        const sub = document.createElement('div');
        sub.style.fontSize = '0.72rem';
        sub.style.color = 'var(--text-muted)';
        sub.textContent = 'Agents: ' + (tool.agents.join(', ') || 'Tous');

        left.appendChild(name);
        left.appendChild(sub);

        const badge = document.createElement('span');
        badge.className = 'mono';
        badge.style.fontSize = '0.78rem';
        badge.style.color = 'var(--accent-purple)';
        badge.textContent = tool.invocations + ' appel(s)';

        div.appendChild(left);
        div.appendChild(badge);
        container.appendChild(div);
      });
    }

    function copySnippet(text) {
      navigator.clipboard.writeText(text).then(() => {
        alert('Copié dans le presse-papier !');
      });
    }

    // Refresh control
    const refreshSelect = document.getElementById('refreshInterval');
    const refreshBtn = document.getElementById('refreshBtn');

    function setupRefresh() {
      if (refreshTimer) clearInterval(refreshTimer);
      const val = parseInt(refreshSelect.value, 10);
      if (val > 0) {
        refreshTimer = setInterval(fetchStats, val);
      }
    }

    refreshSelect.addEventListener('change', setupRefresh);
    refreshBtn.addEventListener('click', fetchStats);

    // Logout
    document.getElementById('logoutBtn').addEventListener('click', async () => {
      await fetch('/api/auth/logout', { method: 'POST' });
      window.location.href = '/login';
    });

    // Modal & JSON Export Logic
    let currentSelectedAgent = null;

    function openAgentModal(agent) {
      currentSelectedAgent = agent;
      document.getElementById('modalAgentName').textContent = agent.name;
      document.getElementById('modalAgentDesc').textContent = agent.description || 'Agent spécialisé Section 9';
      document.getElementById('modalModelBadge').textContent = agent.model;

      // Instructions / System prompt
      const promptEl = document.getElementById('modalPromptContent');
      if (agent.system_prompt && agent.system_prompt.trim().length > 0) {
        promptEl.textContent = agent.system_prompt;
        promptEl.style.color = '#e2e8f0';
        promptEl.style.fontStyle = 'normal';
      } else {
        promptEl.textContent = "Aucune instruction personnalisée spécifiée (l'agent s'exécute avec les directives système par défaut).";
        promptEl.style.color = 'var(--text-muted)';
        promptEl.style.fontStyle = 'italic';
      }

      // JSON Configuration export for IDE
      const exportData = {
        name: agent.name,
        description: agent.description,
        model: agent.model,
        system_prompt: agent.system_prompt || null,
        mcp_servers: agent.mcp_servers || [],
        max_iterations: agent.max_iterations || 10,
        env: agent.env || {},
        tools_exposed: agent.tools_exposed || []
      };

      const jsonString = JSON.stringify(exportData, null, 2);
      document.getElementById('modalJsonContent').textContent = jsonString;

      // Parameters
      document.getElementById('metaModel').textContent = agent.model;
      document.getElementById('metaMaxIterations').textContent = (agent.max_iterations || 10) + ' étapes max';
      document.getElementById('metaRuns').textContent = agent.total_runs + ' (Succès: ' + agent.success_count + ', Échecs: ' + agent.failure_count + ')';
      
      const completed = agent.success_count + agent.failure_count;
      const rate = completed === 0 ? 100 : Math.round((agent.success_count / completed) * 100);
      document.getElementById('metaSuccessRate').textContent = rate + '%';

      // MCP Servers
      const mcpEl = document.getElementById('metaMcpServers');
      mcpEl.replaceChildren();
      if (agent.mcp_servers && agent.mcp_servers.length > 0) {
        agent.mcp_servers.forEach(srv => {
          const srvDiv = document.createElement('div');
          srvDiv.style.marginBottom = '0.5rem';
          const cmdArgs = srv.args && srv.args.length > 0 ? ' ' + srv.args.join(' ') : '';
          srvDiv.innerHTML = '<strong style="color:var(--accent-cyan);">' + srv.name + '</strong>: <span class="mono">' + srv.command + cmdArgs + '</span>';
          mcpEl.appendChild(srvDiv);
        });
      } else if (agent.tools_exposed && agent.tools_exposed.length > 0) {
        mcpEl.textContent = 'Outils déclarés: ' + agent.tools_exposed.join(', ');
      } else {
        mcpEl.textContent = 'Aucun serveur MCP configuré.';
      }

      // Env vars
      const envEl = document.getElementById('metaEnvVars');
      const envKeys = agent.env ? Object.keys(agent.env) : [];
      if (envKeys.length > 0) {
        envEl.textContent = envKeys.map(k => k + ' = ' + agent.env[k]).join('\n');
      } else {
        envEl.textContent = 'Aucune variable d\'environnement spécifique.';
      }

      // Switch to first tab
      switchModalTab('tab-prompt');

      // Open overlay
      document.getElementById('agentModal').classList.add('active');
    }

    function closeAgentModal() {
      document.getElementById('agentModal').classList.remove('active');
    }

    function switchModalTab(tabId) {
      document.querySelectorAll('.modal-tab').forEach(b => {
        b.classList.toggle('active', b.getAttribute('data-tab') === tabId);
      });
      document.querySelectorAll('.modal-section').forEach(sec => {
        sec.classList.toggle('active', sec.id === tabId);
      });
    }

    function copyTextWithFeedback(button, text, successLabel) {
      if (!text) return;
      navigator.clipboard.writeText(text).then(() => {
        const orig = button.textContent;
        button.textContent = '✓ ' + (successLabel || 'Copié !');
        button.style.borderColor = 'var(--emerald)';
        button.style.color = 'var(--emerald)';
        setTimeout(() => {
          button.textContent = orig;
          button.style.borderColor = '';
          button.style.color = '';
        }, 2000);
      }).catch(err => {
        console.error('Erreur copie:', err);
        alert('Impossible de copier dans le presse-papier.');
      });
    }

    function downloadAgentJson(agent) {
      if (!agent) return;
      const exportData = {
        name: agent.name,
        description: agent.description,
        model: agent.model,
        system_prompt: agent.system_prompt || null,
        mcp_servers: agent.mcp_servers || [],
        max_iterations: agent.max_iterations || 10,
        env: agent.env || {},
        tools_exposed: agent.tools_exposed || []
      };
      const jsonStr = JSON.stringify(exportData, null, 2);
      const blob = new Blob([jsonStr], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = agent.name + '-config.json';
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
    }

    // Modal Events Setup
    document.getElementById('modalCloseBtn').addEventListener('click', closeAgentModal);
    document.getElementById('btnCloseModalFooter').addEventListener('click', closeAgentModal);
    document.getElementById('agentModal').addEventListener('click', (e) => {
      if (e.target.id === 'agentModal') closeAgentModal();
    });
    document.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') closeAgentModal();
    });

    document.querySelectorAll('.modal-tab').forEach(btn => {
      btn.addEventListener('click', () => switchModalTab(btn.getAttribute('data-tab')));
    });

    document.getElementById('btnCopyPrompt').addEventListener('click', function() {
      if (currentSelectedAgent) {
        copyTextWithFeedback(this, currentSelectedAgent.system_prompt || '', 'Instructions Copiées');
      }
    });
    document.getElementById('btnCopyInstructionsFooter').addEventListener('click', function() {
      if (currentSelectedAgent) {
        copyTextWithFeedback(this, currentSelectedAgent.system_prompt || '', 'Instructions Copiées');
      }
    });

    document.getElementById('btnCopyJsonTab').addEventListener('click', function() {
      const code = document.getElementById('modalJsonContent').textContent;
      copyTextWithFeedback(this, code, 'JSON Copié');
    });
    document.getElementById('btnCopyJsonFooter').addEventListener('click', function() {
      const code = document.getElementById('modalJsonContent').textContent;
      copyTextWithFeedback(this, code, 'JSON Copié');
    });

    document.getElementById('btnDownloadJsonTab').addEventListener('click', function() {
      if (currentSelectedAgent) downloadAgentJson(currentSelectedAgent);
    });
    document.getElementById('btnDownloadJsonFooter').addEventListener('click', function() {
      if (currentSelectedAgent) downloadAgentJson(currentSelectedAgent);
    });

    // Initial load
    fetchStats();
    setupRefresh();
  </script>
</body>
</html>
"#;
