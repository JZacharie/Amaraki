use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

#[derive(Clone)]
pub struct SlackNotifier {
    client: reqwest::Client,
    token: Option<String>,
    ai_channel_id: Option<String>,
    joe_user_id: Option<String>,
    channel_cache: Arc<RwLock<HashMap<String, String>>>,
    user_cache: Arc<RwLock<HashMap<String, bool>>>,
}

#[derive(Serialize)]
struct PostMessagePayload<'a> {
    channel: &'a str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_ts: Option<&'a str>,
    username: &'a str,
    icon_url: &'a str,
}

impl SlackNotifier {
    pub fn new() -> Self {
        let token = std::env::var("SLACK_BOT_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty());
        if token.is_some() {
            info!("[SLACK] ✅ SLACK_BOT_TOKEN détecté et actif pour Chef Amaraki");
        } else {
            info!(
                "[SLACK] ℹ️ SLACK_BOT_TOKEN non configuré (mode autonome sans notifications Slack)"
            );
        }

        let ai_channel_id = std::env::var("SLACK_AI_CHANNEL_ID")
            .or_else(|_| std::env::var("SLACK_AI_CHANNEL"))
            .or_else(|_| std::env::var("SLACK_CHANNEL_ID"))
            .ok()
            .filter(|c| !c.trim().is_empty());

        let joe_user_id = std::env::var("SLACK_JOE_USER_ID")
            .or_else(|_| std::env::var("SLACK_AUTHORIZED_USERS"))
            .ok()
            .filter(|u| !u.trim().is_empty());

        if let Some(ref ch) = ai_channel_id {
            info!("[SLACK] 🎯 Canal AI configuré : '{}'", ch);
        }
        if let Some(ref u) = joe_user_id {
            info!("[SLACK] 👤 ID utilisateur Joe configuré : '{}'", u);
        }

        Self {
            client: reqwest::Client::new(),
            token,
            ai_channel_id,
            joe_user_id,
            channel_cache: Arc::new(RwLock::new(HashMap::new())),
            user_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn is_configured(&self) -> bool {
        self.token.is_some()
    }

    /// Vérifie si un identifiant de canal correspond au canal AI (nom ou ID)
    pub async fn is_ai_channel(&self, channel_id: &str) -> bool {
        let trimmed = channel_id.trim();
        if trimmed.is_empty() {
            return false;
        }

        // Correspondance directe par nom
        if trimmed.eq_ignore_ascii_case("ai")
            || trimmed.eq_ignore_ascii_case("c_ai")
            || trimmed.eq_ignore_ascii_case("ai-monitoring")
        {
            return true;
        }

        // Correspondance avec l'ID configuré
        if let Some(ref target) = self.ai_channel_id {
            if target == trimmed || target.split(',').any(|c| c.trim() == trimmed) {
                return true;
            }
        }

        // Recherche dans le cache de résolution
        {
            let cache = self.channel_cache.read().await;
            if let Some(name) = cache.get(trimmed) {
                return name.eq_ignore_ascii_case("ai")
                    || name.starts_with("ai-")
                    || name.ends_with("-ai")
                    || name.contains("ai");
            }
        }

        // Résolution dynamique via l'API Slack conversations.info
        if let Some(ref token) = self.token {
            let url = format!(
                "https://slack.com/api/conversations.info?channel={}",
                trimmed
            );
            if let Ok(resp) = self.client.get(&url).bearer_auth(token).send().await {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if json.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
                        let name = json
                            .get("channel")
                            .and_then(|c| c.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_lowercase();

                        let is_ai = name == "ai"
                            || name.starts_with("ai-")
                            || name.ends_with("-ai")
                            || name.contains("ai");

                        info!(
                            "[SLACK] 📡 Résolution canal Slack '{}' -> nom=\"{}\" (is_ai_channel={})",
                            trimmed, name, is_ai
                        );

                        let mut cache = self.channel_cache.write().await;
                        cache.insert(trimmed.to_string(), name);
                        return is_ai;
                    }
                }
            }
        }

        false
    }

    /// Vérifie si un identifiant d'utilisateur correspond à Joe (Joseph Zacharie)
    pub async fn is_joe_user(&self, user_id: &str) -> bool {
        let trimmed = user_id.trim();
        if trimmed.is_empty() || trimmed == "inconnu" {
            return false;
        }

        // Noms explicites ou identifiants directs
        if trimmed.eq_ignore_ascii_case("joe")
            || trimmed.eq_ignore_ascii_case("joseph")
            || trimmed.eq_ignore_ascii_case("jzacharie")
            || trimmed.eq_ignore_ascii_case("u_joe")
        {
            return true;
        }

        // Correspondance avec l'ID configuré
        if let Some(ref target) = self.joe_user_id {
            if target == trimmed || target.split(',').any(|u| u.trim() == trimmed) {
                return true;
            }
        }

        // Recherche dans le cache de résolution
        {
            let cache = self.user_cache.read().await;
            if let Some(&is_joe) = cache.get(trimmed) {
                return is_joe;
            }
        }

        // Résolution dynamique via l'API Slack users.info
        if let Some(ref token) = self.token {
            let url = format!("https://slack.com/api/users.info?user={}", trimmed);
            if let Ok(resp) = self.client.get(&url).bearer_auth(token).send().await {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if json.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
                        if let Some(user_obj) = json.get("user") {
                            let name = user_obj
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_lowercase();
                            let real_name = user_obj
                                .get("real_name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_lowercase();
                            let display_name = user_obj
                                .get("profile")
                                .and_then(|p| p.get("display_name"))
                                .and_then(|d| d.as_str())
                                .unwrap_or("")
                                .to_lowercase();
                            let email = user_obj
                                .get("profile")
                                .and_then(|p| p.get("email"))
                                .and_then(|e| e.as_str())
                                .unwrap_or("")
                                .to_lowercase();
                            let is_primary_owner = user_obj
                                .get("is_primary_owner")
                                .and_then(|b| b.as_bool())
                                .unwrap_or(false);

                            let is_joe = name.contains("joe")
                                || name.contains("joseph")
                                || name.contains("jzacharie")
                                || display_name.contains("joe")
                                || display_name.contains("joseph")
                                || real_name.contains("joseph")
                                || real_name.contains("zacharie")
                                || email.contains("zacharie.org")
                                || email.contains("joseph")
                                || is_primary_owner;

                            info!(
                                "[SLACK] 👤 Résolution profil Slack '{}' -> name=\"{}\", real_name=\"{}\", email=\"{}\", is_joe={}",
                                trimmed, name, real_name, email, is_joe
                            );

                            let mut cache = self.user_cache.write().await;
                            cache.insert(trimmed.to_string(), is_joe);
                            return is_joe;
                        }
                    }
                }
            }
        }

        false
    }

    pub async fn post_message(&self, channel: &str, text: &str, thread_ts: Option<&str>) -> bool {
        let token = match &self.token {
            Some(t) => t,
            None => {
                warn!(
                    "[SLACK] Tentative d'envoi sans SLACK_BOT_TOKEN vers '{}'",
                    channel
                );
                return false;
            }
        };

        let payload = PostMessagePayload {
            channel,
            text,
            thread_ts,
            username: "Chef Amaraki",
            icon_url: "https://amaraki.p.zacharie.org/logo.png",
        };

        match self
            .client
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(token)
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    let v: serde_json::Value = resp.json().await.unwrap_or_default();
                    if v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
                        info!("[SLACK] 💬 Message envoyé avec succès dans '{}'", channel);
                        return true;
                    } else {
                        let err_str = v.get("error").and_then(|e| e.as_str()).unwrap_or("");
                        error!(
                            "[SLACK] ❌ Échec API Slack (chat.postMessage): {:?}",
                            v.get("error")
                        );
                        if thread_ts.is_some()
                            && (err_str == "channel_not_found" || err_str == "thread_not_found")
                        {
                            warn!("[SLACK] ⚠️ Échec avec thread_ts, tentative de repli direct sans thread...");
                            let fallback_payload = PostMessagePayload {
                                channel,
                                text,
                                thread_ts: None,
                                username: "Chef Amaraki",
                                icon_url: "https://amaraki.p.zacharie.org/logo.png",
                            };
                            if let Ok(retry_resp) = self
                                .client
                                .post("https://slack.com/api/chat.postMessage")
                                .bearer_auth(token)
                                .json(&fallback_payload)
                                .send()
                                .await
                            {
                                let rv: serde_json::Value =
                                    retry_resp.json().await.unwrap_or_default();
                                if rv.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
                                    info!("[SLACK] 💬 Message envoyé avec succès (repli hors-thread) dans '{}'", channel);
                                    return true;
                                }
                            }
                        }
                    }
                } else {
                    error!("[SLACK] ❌ Échec HTTP Slack: {}", resp.status());
                }
                false
            }
            Err(e) => {
                error!("[SLACK] ❌ Erreur réseau envoi Slack: {}", e);
                false
            }
        }
    }
}
