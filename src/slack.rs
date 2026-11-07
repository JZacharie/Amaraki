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

        // Correspondance directe par nom ou ID Slack connu
        if trimmed.eq_ignore_ascii_case("ai")
            || trimmed.eq_ignore_ascii_case("c_ai")
            || trimmed.eq_ignore_ascii_case("ai-monitoring")
            || trimmed == "C0AC8NGNESY"
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
            || trimmed == "U29TP96P8"
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
                    } else {
                        let err_msg = json
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("unknown");
                        tracing::debug!(
                            "[SLACK] users.info pour '{}' a échoué (error: {})",
                            trimmed,
                            err_msg
                        );
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

    /// Téléverse un fichier audio dans un canal ou fil Slack via la nouvelle API files.getUploadURLExternal / files.completeUploadExternal
    pub async fn upload_audio(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        audio_bytes: &[u8],
        filename: &str,
        title: &str,
    ) -> bool {
        let token = match &self.token {
            Some(t) => t,
            None => {
                warn!("[SLACK] Téléversement audio ignoré : SLACK_BOT_TOKEN non configuré");
                return false;
            }
        };

        if audio_bytes.is_empty() {
            warn!("[SLACK] Téléversement audio ignoré : tampon audio vide");
            return false;
        }

        // Étape 1 : Obtenir l'URL de téléversement externe Slack
        let get_url = format!(
            "https://slack.com/api/files.getUploadURLExternal?filename={}&length={}",
            filename,
            audio_bytes.len()
        );

        let step1_resp = match self
            .client
            .get(&get_url)
            .bearer_auth(token)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!("[SLACK] ❌ Échec files.getUploadURLExternal: {}", e);
                return false;
            }
        };

        let step1_json: serde_json::Value = match step1_resp.json().await {
            Ok(j) => j,
            Err(e) => {
                error!("[SLACK] ❌ Erreur parsing réponse getUploadURLExternal: {}", e);
                return false;
            }
        };

        if !step1_json.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
            error!(
                "[SLACK] ❌ Échec API files.getUploadURLExternal: {:?}",
                step1_json.get("error")
            );
            return false;
        }

        let upload_url = match step1_json.get("upload_url").and_then(|u| u.as_str()) {
            Some(u) => u,
            None => {
                error!("[SLACK] ❌ upload_url manquant dans la réponse Slack");
                return false;
            }
        };

        let file_id = match step1_json.get("file_id").and_then(|f| f.as_str()) {
            Some(f) => f,
            None => {
                error!("[SLACK] ❌ file_id manquant dans la réponse Slack");
                return false;
            }
        };

        // Étape 2 : POST des octets bruts vers l'URL dédiée
        let step2_resp = match self
            .client
            .post(upload_url)
            .header("Content-Type", "audio/wav")
            .body(audio_bytes.to_vec())
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!("[SLACK] ❌ Échec POST des données audio vers Slack: {}", e);
                return false;
            }
        };

        if !step2_resp.status().is_success() {
            error!(
                "[SLACK] ❌ Échec téléversement payload audio: HTTP {}",
                step2_resp.status()
            );
            return false;
        }

        // Étape 3 : Finaliser le téléversement et attacher le fichier au canal/thread
        let complete_payload = serde_json::json!({
            "files": [
                {
                    "id": file_id,
                    "title": title
                }
            ],
            "channel_id": channel,
            "thread_ts": thread_ts
        });

        let step3_resp = match self
            .client
            .post("https://slack.com/api/files.completeUploadExternal")
            .bearer_auth(token)
            .json(&complete_payload)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!("[SLACK] ❌ Échec files.completeUploadExternal: {}", e);
                return false;
            }
        };

        let step3_json: serde_json::Value = match step3_resp.json().await {
            Ok(j) => j,
            Err(e) => {
                error!("[SLACK] ❌ Erreur parsing files.completeUploadExternal: {}", e);
                return false;
            }
        };

        if step3_json.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
            info!(
                "[SLACK] 🎙️ Audio envoyé avec succès dans '{}' (thread: {:?}, file_id: {})",
                channel, thread_ts, file_id
            );
            true
        } else {
            error!(
                "[SLACK] ❌ Échec finalisation files.completeUploadExternal: {:?}",
                step3_json.get("error")
            );
            false
        }
    }
}
