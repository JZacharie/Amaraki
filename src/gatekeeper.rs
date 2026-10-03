use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

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
    whisper_api_key: Option<String>,
    whisper_model: String,
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
        let whisper_api_key = std::env::var("WHISPER_API_KEY")
            .or_else(|_| std::env::var("GROQ_API_KEY"))
            .ok()
            .filter(|k| !k.trim().is_empty());

        let whisper_url = std::env::var("WHISPER_URL").unwrap_or_else(|_| {
            if whisper_api_key.is_some() {
                "https://api.groq.com/openai/v1/audio/transcriptions".to_string()
            } else {
                "http://whisperx-http.whisperx.svc.cluster.local:8080/asr".to_string()
            }
        });

        let whisper_model = std::env::var("WHISPER_MODEL").unwrap_or_else(|_| {
            if whisper_api_key.is_some() || whisper_url.contains("groq.com") {
                "whisper-large-v3-turbo".to_string()
            } else {
                "whisper-1".to_string()
            }
        });

        if whisper_api_key.is_some() {
            info!(
                "[GATEKEEPER] 🎙️ Service STT configuré avec API externe ({}) | Modèle: {}",
                whisper_url, whisper_model
            );
        } else {
            info!(
                "[GATEKEEPER] 🎙️ Service STT configuré sur endpoint local ({}) | Modèle: {}",
                whisper_url, whisper_model
            );
        }

        Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
            http_client: reqwest::Client::new(),
            whisper_url,
            whisper_api_key,
            whisper_model,
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
    /// 4. Message from Joe in the AI Slack channel
    #[allow(clippy::too_many_arguments)]
    pub async fn should_trigger(
        &self,
        event_type: &str,
        text: &str,
        channel: &str,
        user_id: &str,
        thread_ts: Option<&str>,
        has_media: bool,
        slack: Option<&crate::slack::SlackNotifier>,
    ) -> bool {
        // Condition 0: Message direct (DM / IM) dans l'onglet messages
        if channel.starts_with('D') {
            return true;
        }

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

        // Condition 4: Messages de Joe sur le canal AI de Slack
        if let Some(s) = slack {
            if s.is_ai_channel(channel).await && s.is_joe_user(user_id).await {
                info!(
                    "[GATEKEEPER] 🎯 Message de Joe intercepté sur le canal AI '{}' (user: '{}') -> activation de l'agent",
                    channel, user_id
                );
                return true;
            }
        } else {
            let is_ai = channel.eq_ignore_ascii_case("ai")
                || channel.contains("ai")
                || std::env::var("SLACK_AI_CHANNEL_ID").map(|c| c == channel).unwrap_or(false);
            let is_joe = user_id.eq_ignore_ascii_case("joe")
                || user_id.eq_ignore_ascii_case("joseph")
                || std::env::var("SLACK_JOE_USER_ID").map(|u| u == user_id).unwrap_or(false);
            if is_ai && is_joe {
                return true;
            }
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

        // 0. Détection d'un nom d'agent explicitement ciblé
        if lower.contains("opencode-leclerc") || lower.contains("leclerc-agent") {
            let agent = "opencode-leclerc".to_string();
            let summary = "consulter le statut du panier Leclerc Drive, préparer ou valider la liste de courses".to_string();
            return (agent, summary);
        }
        if lower.contains("opencode-mail") || lower.contains("mail-agent") {
            let agent = "opencode-mail".to_string();
            let summary = "consulter la boîte Gmail, filtrer les urgences et produire une synthèse".to_string();
            return (agent, summary);
        }
        if lower.contains("agent-code-reviewer") || lower.contains("code-reviewer") {
            let agent = "agent-code-reviewer".to_string();
            let summary = "analyser le code source et proposer des optimisations techniques".to_string();
            return (agent, summary);
        }
        if lower.contains("agent-k8s-diagnostician") || lower.contains("k8s-diagnostician") {
            let agent = "agent-k8s-diagnostician".to_string();
            let summary = "diagnostiquer l'état des pods et analyser les anomalies du cluster Kubernetes".to_string();
            return (agent, summary);
        }
        if lower.contains("agent-incident-responder") || lower.contains("incident-responder") {
            let agent = "agent-incident-responder".to_string();
            let summary = "coordonner l'investigation et la réponse à l'incident critique".to_string();
            return (agent, summary);
        }

        // 1. Leclerc Drive & Courses
        if lower.contains("leclerc")
            || lower.contains("panier")
            || lower.contains("course")
            || lower.contains("courses")
            || lower.contains("drive")
        {
            let agent = "opencode-leclerc".to_string();
            let summary = "consulter le statut du panier Leclerc Drive, vérifier son contenu, préparer ou valider la liste de courses".to_string();
            return (agent, summary);
        }

        // 2. Mail & Urgences Mails
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

        let mime = if filename.ends_with(".m4a") {
            "audio/m4a"
        } else if filename.ends_with(".wav") {
            "audio/wav"
        } else if filename.ends_with(".ogg") {
            "audio/ogg"
        } else if filename.ends_with(".mp4") {
            "video/mp4"
        } else {
            "audio/mpeg"
        };

        // Build multipart request compatible with both OpenAI Whisper and WhisperX ASR
        let part1 = reqwest::multipart::Part::bytes(audio_bytes.to_vec())
            .file_name(filename.to_string())
            .mime_str(mime)?;

        let mut form = reqwest::multipart::Form::new()
            .part("file", part1)
            .text("model", self.whisper_model.clone())
            .text("language", "fr");

        if self.whisper_api_key.is_none() {
            let part2 = reqwest::multipart::Part::bytes(audio_bytes.to_vec())
                .file_name(filename.to_string())
                .mime_str(mime)?;
            form = form.part("audio_file", part2);
        }

        let mut req_builder = self.http_client.post(&self.whisper_url).multipart(form);
        if let Some(ref key) = self.whisper_api_key {
            req_builder = req_builder.bearer_auth(key);
        }

        let resp = req_builder.send().await?;

        let status = resp.status();
        if status.is_success() {
            let val: serde_json::Value = resp.json().await?;
            if let Some(text) = val.get("text").and_then(|t| t.as_str()) {
                if !text.trim().is_empty() {
                    info!("[GATEKEEPER] ✅ Transcription réussie : \"{}\"", text);
                    return Ok(text.trim().to_string());
                }
            }
            if let Some(segments) = val.get("segments").and_then(|s| s.as_array()) {
                let joined: String = segments
                    .iter()
                    .filter_map(|s| s.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(" ");
                if !joined.trim().is_empty() {
                    info!(
                        "[GATEKEEPER] ✅ Transcription segments réussie : \"{}\"",
                        joined
                    );
                    return Ok(joined.trim().to_string());
                }
            }
            warn!("[GATEKEEPER] ⚠️ Transcription vide retournée par le service STT");
            Err(anyhow::anyhow!("Transcription vide retournée par le service STT").into())
        } else {
            let err_body = resp.text().await.unwrap_or_default();
            error!(
                "[GATEKEEPER] ❌ Échec de transcription HTTP ({}) : {}",
                status, err_body
            );
            Err(anyhow::anyhow!("Échec HTTP {} du service STT : {}", status, err_body).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_should_trigger_conditions() {
        let gatekeeper = GatekeeperStore::new();

        // 1. Direct mention
        assert!(gatekeeper.should_trigger("app_mention", "hello", "C123", "U_OTHER", None, false, None).await);
        assert!(gatekeeper.should_trigger("message", "hello amaraki", "C123", "U_OTHER", None, false, None).await);

        // 2. Audio/video media
        assert!(gatekeeper.should_trigger("message", "voice", "C123", "U_OTHER", None, true, None).await);

        // 3. DM
        assert!(gatekeeper.should_trigger("message", "hello", "D12345", "U_OTHER", None, false, None).await);

        // 4. Joe in AI channel
        assert!(gatekeeper.should_trigger("message", "relance le pod traefik", "ai", "joe", None, false, None).await);
        assert!(gatekeeper.should_trigger("message", "fais les courses", "ai", "joseph", None, false, None).await);

        // 5. Other user without keyword on non-DM channel should NOT trigger
        assert!(!gatekeeper.should_trigger("message", "coucou tout le monde", "general", "user1", None, false, None).await);
    }

    #[test]
    fn test_analyze_intent_routing() {
        // Direct agent target
        let (agent, _) = GatekeeperStore::analyze_intent("opencode-leclerc: ajoute du café");
        assert_eq!(agent, "opencode-leclerc");

        let (agent, _) = GatekeeperStore::analyze_intent("opencode-mail: résume les mails d'hier");
        assert_eq!(agent, "opencode-mail");

        // Keywords
        let (agent, _) = GatekeeperStore::analyze_intent("ajoute des fruits au panier du drive leclerc");
        assert_eq!(agent, "opencode-leclerc");

        let (agent, _) = GatekeeperStore::analyze_intent("regarde si j'ai reçu un mail urgent");
        assert_eq!(agent, "opencode-mail");

        let (agent, _) = GatekeeperStore::analyze_intent("vérifie les pods du cluster k8s");
        assert_eq!(agent, "agent-k8s-diagnostician");

        let (agent, _) = GatekeeperStore::analyze_intent("fais une review du code de la PR");
        assert_eq!(agent, "agent-code-reviewer");
    }
}
