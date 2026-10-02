use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingValidation {
    pub channel: String,
    pub thread_ts: String,
    pub user: Option<String>,
    pub agent_name: String,
    pub action_summary: String,
    pub full_prompt: String,
    pub awaiting_clarification: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct GatekeeperStore {
    pending: Arc<RwLock<HashMap<String, PendingValidation>>>,
    http_client: reqwest::Client,
    whisper_url: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum UserValidationIntent {
    Yes,
    No,
    Stop,
    NewInstruction(String),
}

impl GatekeeperStore {
    pub fn new() -> Self {
        let whisper_url = std::env::var("WHISPER_URL").unwrap_or_else(|_| {
            "http://speaches.speaches.svc.cluster.local:8000/v1/audio/transcriptions".to_string()
        });
        Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
            http_client: reqwest::Client::new(),
            whisper_url,
        }
    }

    /// Key for an ongoing thread or channel interaction
    pub fn make_key(channel: &str, thread_ts: Option<&str>) -> String {
        match thread_ts {
            Some(ts) if !ts.trim().is_empty() => format!("{}:{}", channel, ts.trim()),
            _ => channel.to_string(),
        }
    }

    pub async fn get_pending(&self, key: &str) -> Option<PendingValidation> {
        let lock = self.pending.read().await;
        lock.get(key).cloned()
    }

    pub async fn set_pending(&self, key: String, val: PendingValidation) {
        let mut lock = self.pending.write().await;
        lock.insert(key, val);
    }

    pub async fn remove_pending(&self, key: &str) -> Option<PendingValidation> {
        let mut lock = self.pending.write().await;
        lock.remove(key)
    }

    /// Check if a message meets activation triggers:
    /// 1. Direct mention (@amaraki or keyword amaraki)
    /// 2. Active participation in an existing thread
    /// 3. Media file (audio/video)
    pub async fn should_trigger(
        &self,
        event_type: &str,
        text: &str,
        channel: &str,
        thread_ts: Option<&str>,
        has_media: bool,
    ) -> bool {
        // Condition 1: Direct app_mention event
        if event_type == "app_mention" {
            return true;
        }

        // Condition 1b: Contains direct mention or keyword @amaraki / amaraki
        let lower = text.to_lowercase();
        if lower.contains("amaraki") || lower.contains("aramaki") {
            return true;
        }

        // Condition 2: Active thread already in validation/discussion
        let key = Self::make_key(channel, thread_ts);
        {
            let lock = self.pending.read().await;
            if lock.contains_key(&key) {
                return true;
            }
        }

        // Condition 3: Audio or video media shared
        if has_media {
            return true;
        }

        false
    }

    /// Parse user reply for validation (Oui, Non, Stop)
    pub fn parse_validation(text: &str) -> UserValidationIntent {
        let trimmed = text.trim().to_lowercase();
        // Remove trailing punctuation or emojis
        let cleaned = trimmed.trim_matches(|c: char| !c.is_alphanumeric()).trim();

        match cleaned {
            "oui" | "yes" | "o" | "ok" | "daccord" | "d'accord" | "vasy" | "vas-y"
            | "confirmer" | "go" => UserValidationIntent::Yes,
            "non" | "no" | "n" | "nope" | "pas daccord" | "pas d'accord" => {
                UserValidationIntent::No
            }
            "stop" | "annuler" | "cancel" | "abandonner" | "arreter" | "arrête" | "quitter" => {
                UserValidationIntent::Stop
            }
            _ => UserValidationIntent::NewInstruction(text.trim().to_string()),
        }
    }

    /// Formulate 1-2 sentence synthetic summary of the action and select the external agent
    pub fn analyze_intent(text: &str) -> (String, String) {
        let lower = text.to_lowercase();

        // 1. Mail & Urgences Mails
        if lower.contains("mail")
            || lower.contains("email")
            || lower.contains("courriel")
            || lower.contains("urgences")
            || lower.contains("gmail")
        {
            let agent = "opencode-mail".to_string();
            let summary = "consulter la boîte Gmail de Joseph ZACHARIE, filtrer les urgences depuis la veille 18h et produire une synthèse concise".to_string();
            return (agent, summary);
        }

        // 2. Code Review & analyse de code
        if lower.contains("code")
            || lower.contains("revue")
            || lower.contains("review")
            || lower.contains("pr")
            || lower.contains("git")
            || lower.contains("refactor")
        {
            let agent = "agent-code-reviewer".to_string();
            let summary = "analyser le code source et proposer des optimisations techniques et architecturales".to_string();
            return (agent, summary);
        }

        // 3. Diagnostic Kubernetes / Pods / Cluster
        if lower.contains("k8s")
            || lower.contains("pod")
            || lower.contains("cluster")
            || lower.contains("crash")
            || lower.contains("anomalie")
            || lower.contains("log")
        {
            let agent = "agent-k8s-diagnostician".to_string();
            let summary =
                "diagnostiquer l'état des pods et analyser les anomalies du cluster Kubernetes"
                    .to_string();
            return (agent, summary);
        }

        // 4. Incidents & alertes critiques
        if lower.contains("incident")
            || lower.contains("alerte")
            || lower.contains("secours")
            || lower.contains("panne")
        {
            let agent = "agent-incident-responder".to_string();
            let summary =
                "coordonner l'investigation et la réponse à l'incident critique".to_string();
            return (agent, summary);
        }

        // 5. Default / Agent général
        let agent = "agent-code-reviewer".to_string();
        let prompt_preview = if text.len() > 80 {
            format!("{}...", &text[..77])
        } else {
            text.to_string()
        };
        let summary = format!(
            "exécuter l'instruction suivante : « {} »",
            prompt_preview.trim()
        );
        (agent, summary)
    }

    /// Download media from Slack and submit to Whisper for transcription
    pub async fn transcribe_slack_media(
        &self,
        download_url: &str,
        slack_token: Option<&str>,
        filename: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        info!(
            "[GATEKEEPER] 📥 Téléchargement média Slack ({}) depuis {}",
            filename, download_url
        );

        let mut req = self.http_client.get(download_url);
        if let Some(token) = slack_token {
            req = req.bearer_auth(token);
        }

        let audio_bytes = req.send().await?.bytes().await?;
        info!(
            "[GATEKEEPER] 🎙️ {} octets récupérés, envoi vers le module de transcription {}",
            audio_bytes.len(),
            self.whisper_url
        );

        // Build multipart request for OpenAI-compatible Whisper ASR
        let part = reqwest::multipart::Part::bytes(audio_bytes.to_vec())
            .file_name(filename.to_string())
            .mime_str("audio/mpeg")?;

        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", "whisper-1")
            .text("language", "fr");

        let resp = self
            .http_client
            .post(&self.whisper_url)
            .multipart(form)
            .send()
            .await?;

        let status = resp.status();
        if status.is_success() {
            let val: serde_json::Value = resp.json().await?;
            if let Some(text) = val.get("text").and_then(|t| t.as_str()) {
                info!("[GATEKEEPER] ✅ Transcription réussie : \"{}\"", text);
                return Ok(text.trim().to_string());
            }
        }

        warn!(
            "[GATEKEEPER] ⚠️ Échec de transcription HTTP ({}), fallback sur nom de fichier",
            status
        );
        Ok(format!(
            "Instruction vocale reçue depuis le fichier audio {}",
            filename
        ))
    }
}
