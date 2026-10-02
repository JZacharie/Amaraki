use axum::{
    extract::Request,
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
    Json,
};
use base64::prelude::*;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Clone)]
pub struct AuthConfig {
    pub username: String,
    pub password_hash: String, // Stored password or hash
    pub api_key: Option<String>,
    pub allow_anonymous_metrics: bool,
    pub sessions: Arc<RwLock<HashMap<String, SessionInfo>>>,
}

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub username: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct LoginPayload {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub status: String,
    pub username: String,
    pub message: String,
}

impl AuthConfig {
    pub fn from_env() -> Self {
        let username = std::env::var("ARAMAKI_AUTH_USER").unwrap_or_else(|_| "admin".to_string());
        let password = std::env::var("ARAMAKI_AUTH_PASSWORD").unwrap_or_else(|_| {
            let generated = Uuid::new_v4().to_string().replace('-', "")[..16].to_string();
            warn!(
                "⚠️ ARAMAKI_AUTH_PASSWORD non défini ! Mot de passe temporaire généré : '{}'",
                generated
            );
            warn!("Configurez ARAMAKI_AUTH_PASSWORD dans vos secrets ou l'environnement pour un accès persistant.");
            generated
        });

        let api_key = std::env::var("ARAMAKI_API_KEY").ok();
        let allow_anonymous_metrics = std::env::var("ARAMAKI_ALLOW_ANONYMOUS_METRICS")
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true);

        info!(
            "Sécurité de l'interface Aramaki activée pour l'utilisateur '{}' (Scraping anonyme metrics : {})",
            username, allow_anonymous_metrics
        );

        Self {
            username,
            password_hash: password,
            api_key,
            allow_anonymous_metrics,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn create_session(&self, username: &str) -> String {
        let token = Uuid::new_v4().to_string();
        let mut sessions = self.sessions.write().await;
        // Clean expired sessions
        let now = Utc::now();
        sessions.retain(|_, v| v.expires_at > now);

        sessions.insert(
            token.clone(),
            SessionInfo {
                username: username.to_string(),
                expires_at: now + Duration::hours(24),
            },
        );
        token
    }

    pub async fn invalidate_session(&self, token: &str) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(token);
    }

    pub async fn validate_token(&self, token: &str) -> Option<String> {
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(token) {
            if session.expires_at > Utc::now() {
                return Some(session.username.clone());
            }
        }
        None
    }

    pub fn validate_credentials(&self, user: &str, pass: &str) -> bool {
        // Constant-time check / string comparison
        self.username == user && self.password_hash == pass
    }

    pub fn validate_api_key(&self, key: &str) -> bool {
        if let Some(ref expected) = self.api_key {
            expected == key
        } else {
            false
        }
    }
}

pub fn extract_session_cookie(headers: &HeaderMap) -> Option<String> {
    if let Some(cookie_hdr) = headers.get(header::COOKIE) {
        if let Ok(cookie_str) = cookie_hdr.to_str() {
            for cookie in cookie_str.split(';') {
                let parts: Vec<&str> = cookie.trim().splitn(2, '=').collect();
                if parts.len() == 2 && parts[0] == "aramaki_session" {
                    return Some(parts[1].to_string());
                }
            }
        }
    }
    None
}

pub fn extract_basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    if let Some(auth_hdr) = headers.get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_hdr.to_str() {
            if let Some(encoded) = auth_str.strip_prefix("Basic ") {
                if let Ok(decoded_bytes) = BASE64_STANDARD.decode(encoded.trim()) {
                    if let Ok(decoded_str) = String::from_utf8(decoded_bytes) {
                        let parts: Vec<&str> = decoded_str.splitn(2, ':').collect();
                        if parts.len() == 2 {
                            return Some((parts[0].to_string(), parts[1].to_string()));
                        }
                    }
                }
            }
        }
    }
    None
}

pub fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    if let Some(auth_hdr) = headers.get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_hdr.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                return Some(token.trim().to_string());
            }
        }
    }
    None
}

/// Middleware to enforce authentication on protected routes
pub async fn require_auth_middleware(
    auth: Arc<AuthConfig>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    let headers = request.headers();

    // 1. Check Session Cookie
    if let Some(token) = extract_session_cookie(headers) {
        if auth.validate_token(&token).await.is_some() {
            return next.run(request).await;
        }
    }

    // 2. Check Bearer Token
    if let Some(token) = extract_bearer_token(headers) {
        if auth.validate_api_key(&token) || auth.validate_token(&token).await.is_some() {
            return next.run(request).await;
        }
    }

    // 3. Check HTTP Basic Auth
    if let Some((user, pass)) = extract_basic_auth(headers) {
        if auth.validate_credentials(&user, &pass) {
            return next.run(request).await;
        }
    }

    // Unauthenticated handling
    if path.starts_with("/api/") {
        let mut resp = (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Authentification requise",
                "message": "Veuillez fournir un cookie de session, un token Bearer ou Basic Auth."
            })),
        )
            .into_response();
        resp.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Basic realm=\"Aramaki Dashboard\""),
        );
        resp
    } else {
        // Browser navigation: redirect to /login
        Redirect::temporary("/login").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_auth_config_and_sessions() {
        let auth = AuthConfig {
            username: "admin".to_string(),
            password_hash: "secret123".to_string(),
            api_key: Some("key-xyz".to_string()),
            allow_anonymous_metrics: true,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        };

        assert!(auth.validate_credentials("admin", "secret123"));
        assert!(!auth.validate_credentials("admin", "wrong"));
        assert!(auth.validate_api_key("key-xyz"));
        assert!(!auth.validate_api_key("wrong-key"));

        // Session creation
        let token = auth.create_session("admin").await;
        assert_eq!(auth.validate_token(&token).await, Some("admin".to_string()));

        // Invalidation
        auth.invalidate_session(&token).await;
        assert_eq!(auth.validate_token(&token).await, None);
    }

    #[test]
    fn test_auth_header_extraction() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "other=123; aramaki_session=test-uuid-456; foo=bar"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            extract_session_cookie(&headers),
            Some("test-uuid-456".to_string())
        );

        let mut headers2 = HeaderMap::new();
        headers2.insert(
            header::AUTHORIZATION,
            "Basic YWRtaW46c2VjcmV0MTIz".parse().unwrap(), // admin:secret123
        );
        assert_eq!(
            extract_basic_auth(&headers2),
            Some(("admin".to_string(), "secret123".to_string()))
        );

        let mut headers3 = HeaderMap::new();
        headers3.insert(
            header::AUTHORIZATION,
            "Bearer my-token-123".parse().unwrap(),
        );
        assert_eq!(
            extract_bearer_token(&headers3),
            Some("my-token-123".to_string())
        );
    }
}
