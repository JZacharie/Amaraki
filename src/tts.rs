use serde::Serialize;
use tracing::{error, info};

#[derive(Clone)]
pub struct GradiumTts {
    client: reqwest::Client,
    api_key: Option<String>,
    voice_id: String,
    url: String,
}

#[derive(Serialize)]
struct TtsRequest<'a> {
    text: &'a str,
    voice_id: &'a str,
    output_format: &'a str,
    only_audio: bool,
}

impl GradiumTts {
    pub fn new() -> Self {
        let api_key = std::env::var("GRADIUM_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty());

        let voice_id = std::env::var("GRADIUM_VOICE_ID")
            .unwrap_or_else(|_| "ZvbRby2gYSWbfLyP".to_string());

        let url = std::env::var("GRADIUM_URL")
            .unwrap_or_else(|_| "https://api.gradium.ai/api/post/speech/tts".to_string());

        if api_key.is_some() {
            info!(
                "[TTS] ✅ Gradium TTS actif (voix: '{}', url: '{}')",
                voice_id, url
            );
        } else {
            info!("[TTS] ℹ️ GRADIUM_API_KEY non configuré (synthèse vocale désactivée)");
        }

        Self {
            client: reqwest::Client::new(),
            api_key,
            voice_id,
            url,
        }
    }

    pub fn is_configured(&self) -> bool {
        self.api_key.is_some()
    }

    pub fn voice_id(&self) -> &str {
        &self.voice_id
    }

    /// Nettoie et prépare le texte pour une diction naturelle (retrait du markdown, code blocks, etc.)
    pub fn clean_text_for_speech(raw: &str) -> String {
        let mut text = raw.to_string();

        // 1. Remplacement des blocs de code ```...``` par un résumé audio concis
        while let Some(start) = text.find("```") {
            if let Some(end) = text[start + 3..].find("```") {
                let actual_end = start + 3 + end + 3;
                text.replace_range(start..actual_end, " détail technique disponible dans le fil. ");
            } else {
                text.replace_range(start.., "");
                break;
            }
        }

        // 2. Nettoyage des liens Markdown [titre](url) -> titre
        while let Some(start) = text.find('[') {
            if let Some(mid) = text[start..].find("](") {
                let actual_mid = start + mid;
                if let Some(end) = text[actual_mid..].find(')') {
                    let actual_end = actual_mid + end + 1;
                    let title = text[start + 1..actual_mid].to_string();
                    text.replace_range(start..actual_end, &title);
                    continue;
                }
            }
            break;
        }

        // 3. Suppression des mentions Slack <@U...> ou <#C...>
        let mut cleaned = String::with_capacity(text.len());
        let mut in_bracket = false;
        for ch in text.chars() {
            if ch == '<' {
                in_bracket = true;
            } else if ch == '>' && in_bracket {
                in_bracket = false;
            } else if !in_bracket {
                cleaned.push(ch);
            }
        }

        // 4. Retrait des astérisques, tildes, backticks et puces
        let cleaned = cleaned
            .replace(['*', '`', '_', '~', '#'], "")
            .replace("•", "-");

        // 5. Retrait des émojis courants pour éviter que le TTS ne tente de les prononcer bizarrement
        let cleaned: String = cleaned
            .chars()
            .filter(|&c| {
                let u = c as u32;
                // Exclut les blocs d'émoticônes et symboles divers
                !(0x1F300..=0x1FAFF).contains(&u) && !(0x2600..=0x27BF).contains(&u)
            })
            .collect();

        // 6. Compactage des espaces multiples
        let mut result = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");

        // Limite à ~450 caractères pour garder la réponse vocale percutante et rapide
        if result.len() > 450 {
            if let Some(last_period) = result[..450].rfind(['.', '!', '?']) {
                result.truncate(last_period + 1);
            } else {
                result.truncate(450);
                result.push('.');
            }
        }

        result
    }

    /// Synthétise un texte en audio WAV via l'API Gradium
    pub async fn synthesize(
        &self,
        text: &str,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let api_key = match &self.api_key {
            Some(k) => k,
            None => {
                return Err("GRADIUM_API_KEY non disponible".into());
            }
        };

        let speech_text = Self::clean_text_for_speech(text);
        if speech_text.trim().is_empty() {
            return Err("Texte vide après nettoyage pour TTS".into());
        }

        let payload = TtsRequest {
            text: &speech_text,
            voice_id: &self.voice_id,
            output_format: "wav",
            only_audio: true,
        };

        info!(
            "[TTS] 🗣️ Synthèse vocale Gradium en cours (voix: {}, {} car): \"{}\"",
            self.voice_id,
            speech_text.len(),
            if speech_text.len() > 60 {
                format!("{}...", &speech_text[..57])
            } else {
                speech_text.clone()
            }
        );

        let resp = self
            .client
            .post(&self.url)
            .header("x-api-key", api_key)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            error!(
                "[TTS] ❌ Échec API Gradium (HTTP {}): {}",
                status, err_body
            );
            return Err(format!("Gradium TTS failed with HTTP {}: {}", status, err_body).into());
        }

        let bytes = resp.bytes().await?;
        info!(
            "[TTS] 🎶 Audio généré avec succès (taille: {} octets)",
            bytes.len()
        );
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_text_for_speech() {
        let raw = "🏁 *Chef Amaraki* : Mission terminée pour `agent-test`!\n```\nresult = 42\n```\nConsultez [Wiki](https://wiki.org) ou <@U12345> • Bravo! 🎉";
        let cleaned = GradiumTts::clean_text_for_speech(raw);
        assert!(!cleaned.contains("```"));
        assert!(!cleaned.contains("result = 42"));
        assert!(!cleaned.contains('*'));
        assert!(!cleaned.contains('`'));
        assert!(!cleaned.contains("https://"));
        assert!(!cleaned.contains("<@U12345>"));
        assert!(cleaned.contains("Chef Amaraki"));
        assert!(cleaned.contains("Wiki"));
        assert!(cleaned.contains("détail technique"));
    }

    #[test]
    fn test_default_voice_and_config() {
        let tts = GradiumTts::new();
        assert_eq!(tts.voice_id(), "ZvbRby2gYSWbfLyP");
    }
}
