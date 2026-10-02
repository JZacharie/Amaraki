use serde::Serialize;
use tracing::{error, info, warn};

#[derive(Clone)]
pub struct SlackNotifier {
    client: reqwest::Client,
    token: Option<String>,
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
            info!("[SLACK] ✅ SLACK_BOT_TOKEN détecté et actif pour Chef Aramaki");
        } else {
            info!(
                "[SLACK] ℹ️ SLACK_BOT_TOKEN non configuré (mode autonome sans notifications Slack)"
            );
        }
        Self {
            client: reqwest::Client::new(),
            token,
        }
    }

    pub fn is_configured(&self) -> bool {
        self.token.is_some()
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
            username: "Chef Aramaki",
            icon_url: "https://aramaki.p.zacharie.org/logo.png",
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
                                username: "Chef Aramaki",
                                icon_url: "https://aramaki.p.zacharie.org/logo.png",
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
