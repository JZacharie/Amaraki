use crate::metrics::MetricsStore;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::ListParams;
use kube::Api;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfigMapData {
    pub name: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub mcp_servers: Option<Vec<McpServerConfig>>,
    pub max_iterations: Option<u32>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

pub async fn check_agent_configmap_exists(
    client: &kube::Client,
    ns: &str,
    agent_name: &str,
) -> Result<bool, kube::Error> {
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), ns);
    match cms.get(agent_name).await {
        Ok(_) => Ok(true),
        Err(kube::Error::Api(e)) if e.code == 404 => {
            let safe_name = agent_name.to_lowercase().replace('_', "-");
            if safe_name != agent_name {
                match cms.get(&safe_name).await {
                    Ok(_) => Ok(true),
                    Err(kube::Error::Api(e2)) if e2.code == 404 => Ok(false),
                    Err(e2) => Err(e2),
                }
            } else {
                Ok(false)
            }
        }
        Err(e) => Err(e),
    }
}

pub async fn get_agent_config(
    client: &kube::Client,
    ns: &str,
    agent_name: &str,
) -> Result<Option<AgentConfigMapData>, kube::Error> {
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), ns);
    let cm = match cms.get(agent_name).await {
        Ok(c) => Some(c),
        Err(kube::Error::Api(e)) if e.code == 404 => {
            let safe_name = agent_name.to_lowercase().replace('_', "-");
            if safe_name != agent_name {
                cms.get(&safe_name).await.ok()
            } else {
                None
            }
        }
        Err(e) => return Err(e),
    };

    if let Some(cm) = cm {
        if let Some(data) = cm.data {
            if let Some(raw_json) = data.get("agent.json") {
                match serde_json::from_str::<AgentConfigMapData>(raw_json) {
                    Ok(config) => return Ok(Some(config)),
                    Err(e) => warn!(
                        "Erreur désérialisation agent.json pour {}: {}",
                        agent_name, e
                    ),
                }
            }
        }
    }
    Ok(None)
}

pub async fn discover_all_agents(
    client: &kube::Client,
    ns: &str,
    metrics: &MetricsStore,
) -> Result<usize, kube::Error> {
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), ns);
    let lp = ListParams::default();
    let list = cms.list(&lp).await?;
    let mut count = 0;

    for cm in list.items {
        let name = cm.metadata.name.unwrap_or_default();
        if let Some(data) = cm.data {
            if let Some(raw_json) = data.get("agent.json") {
                if let Ok(config) = serde_json::from_str::<AgentConfigMapData>(raw_json) {
                    let agent_name = if !config.name.is_empty() {
                        config.name
                    } else {
                        name.clone()
                    };
                    let model = config
                        .model
                        .unwrap_or_else(|| "opencode/free-default-model".to_string());
                    let desc = config
                        .description
                        .unwrap_or_else(|| "Section 9 Specialist Agent".to_string());
                    let tools: Vec<String> = config
                        .mcp_servers
                        .as_ref()
                        .map(|servers| servers.iter().map(|s| s.name.clone()).collect())
                        .unwrap_or_default();

                    metrics
                        .register_agent_full(
                            &agent_name,
                            &model,
                            &desc,
                            tools,
                            config.system_prompt,
                            config.mcp_servers,
                            config.max_iterations,
                            config.env,
                        )
                        .await;
                    count += 1;
                }
            }
        }
    }

    Ok(count)
}

#[allow(clippy::too_many_arguments)]
pub async fn spawn_agent_job(
    client: &kube::Client,
    ns: &str,
    agent_runner_image: &str,
    agent_name: &str,
    prompt: &str,
    channel: &str,
    thread_ts: &str,
    metrics: &MetricsStore,
) -> Result<String, kube::Error> {
    let jobs: Api<Job> = Api::namespaced(client.clone(), ns);
    let short_uuid = &uuid::Uuid::new_v4().to_string()[..8];
    let safe_agent_name = agent_name.to_lowercase().replace('_', "-");
    let job_id = format!("{}-{}", safe_agent_name, short_uuid);

    // Retrieve agent model and config if known
    let maybe_config = get_agent_config(client, ns, agent_name)
        .await
        .ok()
        .flatten();
    let model = maybe_config
        .as_ref()
        .and_then(|cfg| cfg.model.clone())
        .unwrap_or_else(|| "opencode/free-default-model".to_string());

    // Limitation stricte de la taille du prompt (maîtrise de la consommation de tokens)
    let bounded_prompt = if prompt.len() > 2000 {
        format!(
            "{}... [contexte plafonné à 2000 caractères]",
            &prompt[..1950]
        )
    } else {
        prompt.to_string()
    };

    // Plafond strict d'itérations (2 à 3 tours max d'auto-correction, interdiction des boucles infinies)
    let max_iterations = maybe_config
        .as_ref()
        .and_then(|cfg| cfg.max_iterations)
        .unwrap_or(3)
        .min(3);

    let mut env_list = vec![
        json!({ "name": "AGENT_NAME", "value": agent_name }),
        json!({ "name": "AGENT_MODEL", "value": &model }),
        json!({ "name": "USER_PROMPT", "value": bounded_prompt }),
        json!({ "name": "SLACK_CHANNEL", "value": channel }),
        json!({ "name": "SLACK_THREAD_TS", "value": thread_ts }),
        json!({ "name": "AGENT_CONFIG_PATH", "value": "/etc/agent/agent.json" }),
        json!({ "name": "ARAMAKI_JOB_ID", "value": job_id }),
        json!({ "name": "MAX_AGENT_ITERATIONS", "value": max_iterations.to_string() }),
        json!({ "name": "PREVENT_AGENT_RECURSION", "value": "true" }),
    ];

    if let Ok(token) = std::env::var("SLACK_BOT_TOKEN") {
        env_list.push(json!({ "name": "SLACK_BOT_TOKEN", "value": token }));
    }

    if let Some(cfg) = &maybe_config {
        for (k, v) in &cfg.env {
            env_list.push(json!({ "name": k, "value": v }));
        }

        let mut opencode_config = json!({
            "$schema": "https://opencode.ai/config.json"
        });

        if let Some(prompt) = &cfg.system_prompt {
            opencode_config["instructions"] = json!([prompt]);
        }

        if let Some(servers) = &cfg.mcp_servers {
            let mut mcp_map = serde_json::Map::new();
            for s in servers {
                let mut cmd = Vec::new();
                if let Some(c) = &s.command {
                    cmd.push(c.clone());
                }
                cmd.extend(s.args.clone());
                mcp_map.insert(
                    s.name.clone(),
                    json!({
                        "type": "local",
                        "command": cmd
                    }),
                );
            }
            opencode_config["mcp"] = serde_json::Value::Object(mcp_map);
        }

        if let Ok(config_str) = serde_json::to_string(&opencode_config) {
            env_list.push(json!({ "name": "OPENCODE_CONFIG_CONTENT", "value": config_str }));
        }
    }

    let env_from = vec![
        json!({
            "secretRef": {
                "name": format!("{}-secret", safe_agent_name),
                "optional": true
            }
        }),
        json!({
            "configMapRef": {
                "name": format!("{}-env", safe_agent_name),
                "optional": true
            }
        }),
    ];

    // Determine configMap volume name to mount
    let configmap_vol_name = if check_agent_configmap_exists(client, ns, agent_name)
        .await
        .unwrap_or(false)
    {
        agent_name
    } else {
        &safe_agent_name
    };

    let image_pull_secrets =
        std::env::var("IMAGE_PULL_SECRETS").unwrap_or_else(|_| "regcred".to_string());
    let pull_secrets_vec: Vec<serde_json::Value> = image_pull_secrets
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|name| json!({ "name": name }))
        .collect();

    let service_account_name =
        std::env::var("AGENT_SERVICE_ACCOUNT").unwrap_or_else(|_| "aramaki-sa".to_string());

    let job_manifest: Job = serde_json::from_value(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": job_id,
            "namespace": ns,
            "labels": {
                "app.kubernetes.io/managed-by": "aramaki",
                "agent-name": safe_agent_name
            }
        },
        "spec": {
            "ttlSecondsAfterFinished": 300,
            "template": {
                "spec": {
                    "restartPolicy": "Never",
                    "serviceAccountName": service_account_name,
                    "imagePullSecrets": pull_secrets_vec,
                    "securityContext": {
                        "runAsNonRoot": true,
                        "runAsUser": 1000,
                        "runAsGroup": 1000,
                        "fsGroup": 1000
                    },
                    "containers": [{
                        "name": "opencode-agent",
                        "image": agent_runner_image,
                        "command": ["/bin/sh", "-c"],
                        "args": [
                            r#"
TS_START=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
SEC_START=$(date +%s)
echo "================================================================================"
echo "[$TS_START] [AGENT_INIT] 🚀 Démarrage de l'agent Section 9"
echo "[$TS_START] [AGENT_META] name=\"${AGENT_NAME:-unknown}\" | model=\"${AGENT_MODEL:-unknown}\" | job_id=\"${ARAMAKI_JOB_ID:-unknown}\""
echo "[$TS_START] [INPUT_PROMPT] \"${USER_PROMPT}\""
echo "[$TS_START] [SLACK_CONTEXT] channel=\"${SLACK_CHANNEL:-none}\" | thread_ts=\"${SLACK_THREAD_TS:-none}\""
echo "[$TS_START] [ENV_AUDIT] Variables d'environnement déclarées (secrets masqués) :"
env | cut -d= -f1 | sort | while read -r var_name; do
  case "$var_name" in
    *PASSWORD*|*TOKEN*|*SECRET*|*KEY*|*AUTH*|*COOKIE*)
      echo "  - $var_name=[REDACTED/MASKED]"
      ;;
    *)
      eval "val=\$$var_name"
      if [ ${#val} -gt 120 ]; then
        val="$(echo "$val" | head -c 117)..."
      fi
      echo "  - $var_name=\"$val\""
      ;;
  esac
done
echo "================================================================================"

mkdir -p ~/.config/opencode
if [ -n "$OPENCODE_CONFIG_CONTENT" ]; then
  printf "%s" "$OPENCODE_CONFIG_CONTENT" > ~/.config/opencode/opencode.jsonc
fi

TS_RUN=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
echo "[$TS_RUN] [AGENT_EXEC] ⚡ Exécution d'OpenCode en cours..."

# Exécution de l'agent et capture du code retour
opencode run "$USER_PROMPT"
EXIT_CODE=$?

SEC_END=$(date +%s)
TS_END=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
DURATION=$((SEC_END - SEC_START))

echo "================================================================================"
echo "[$TS_END] [AGENT_COMPLETED] 🏁 Fin d'exécution de l'agent"
echo "[$TS_END] [PERF] Durée totale : ${DURATION}s | Code de sortie : ${EXIT_CODE}"
echo "================================================================================"
exit $EXIT_CODE
"#.trim()
                        ],
                        "securityContext": {
                            "runAsNonRoot": true,
                            "runAsUser": 1000,
                            "allowPrivilegeEscalation": false
                        },
                        "env": env_list,
                        "envFrom": env_from,
                        "volumeMounts": [{
                            "name": "agent-config",
                            "mountPath": "/etc/agent",
                            "readOnly": true
                        }]
                    }],
                    "volumes": [{
                        "name": "agent-config",
                        "configMap": {
                            "name": configmap_vol_name
                        }
                    }]
                }
            }
        }
    }))
    .unwrap();

    jobs.create(&Default::default(), &job_manifest).await?;
    info!(
        "[ACTION] 🚀 Pilotage K8s: Job '{}' créé avec succès pour l'agent '{}' (modèle: '{}', namespace: '{}', canal: '{}')",
        job_id, agent_name, model, ns, channel
    );

    // Record spawn in metrics store
    metrics
        .record_agent_spawn(&job_id, agent_name, &model, channel, prompt)
        .await;

    Ok(job_id)
}

pub async fn sync_jobs(
    client: &kube::Client,
    ns: &str,
    metrics: &MetricsStore,
) -> Result<(), kube::Error> {
    let jobs: Api<Job> = Api::namespaced(client.clone(), ns);
    let lp = ListParams::default().labels("app.kubernetes.io/managed-by=aramaki");

    let job_list = match jobs.list(&lp).await {
        Ok(list) => {
            metrics.set_k8s_connected(true);
            list
        }
        Err(e) => {
            metrics.set_k8s_connected(false);
            return Err(e);
        }
    };

    for job in job_list.items {
        let name = match job.metadata.name {
            Some(n) => n,
            None => continue,
        };

        if let Some(status) = job.status {
            let succeeded = status.succeeded.unwrap_or(0);
            let failed = status.failed.unwrap_or(0);

            if succeeded > 0 {
                // Completed successfully
                let duration = if let (Some(start), Some(complete)) =
                    (status.start_time, status.completion_time)
                {
                    let diff = complete.0.signed_duration_since(start.0);
                    Some(diff.num_milliseconds() as f64 / 1000.0)
                } else {
                    None
                };
                info!(
                    "[ACTION] 🏁 Fin de mission Job K8s: '{}' terminé avec SUCCÈS (durée: {:?}s)",
                    name, duration
                );
                metrics.record_agent_completion(&name, true, duration).await;
            } else if failed > 0 {
                // Completed with failure
                tracing::error!(
                    "[ACTION] 💥 Échec Job K8s: '{}' a ÉCHOUÉ dans le cluster",
                    name
                );
                metrics.record_agent_completion(&name, false, None).await;
            }
        }
    }

    Ok(())
}

/// Fallback to seed realistic default agents for local standalone mode or dashboard preview
pub async fn seed_default_agents(metrics: &MetricsStore) {
    let reviewer_prompt = r#"# Agent « Code Reviewer »

# RÔLE
Tu es un agent expert sous la supervision du Chef Aramaki (Section 9) pour le cluster Kubernetes Jo3.

# MISSION
Analyser les pull requests et le code source, vérifier le respect des bonnes pratiques et de la sécurité (secrets, permissions RBAC, non-root, limites mémoire et CPU), et formuler des revues de code claires, synthétiques et actionnables."#;

    metrics
        .register_agent_full(
            "agent-code-reviewer",
            "opencode/free-default-model",
            "Analyse le code et propose des optimisations sur le cluster Jo3",
            vec!["filesystem".to_string(), "git".to_string()],
            Some(reviewer_prompt.to_string()),
            None,
            Some(5),
            std::collections::HashMap::new(),
        )
        .await;

    metrics
        .register_agent(
            "agent-k8s-diagnostician",
            "deepseek-ai/deepseek-coder",
            "Diagnostique les pods en crashloop et les anomalies cluster",
            vec!["k8s-api".to_string(), "openobserve".to_string()],
        )
        .await;

    metrics
        .register_agent(
            "agent-incident-responder",
            "anthropic/claude-3-5-sonnet",
            "Coordonne les alertes critiques et interventions Section 9",
            vec!["slack".to_string(), "k8s-api".to_string()],
        )
        .await;

    let mut mail_env = std::collections::HashMap::new();
    mail_env.insert("GMAIL_LOGIN".to_string(), "joseph@zacharie.org".to_string());

    let mail_mcp = vec![
        McpServerConfig {
            name: "gmail".to_string(),
            command: Some("npx".to_string()),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-gmail".to_string(),
            ],
        },
        McpServerConfig {
            name: "buzz-dev-mcp".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), "buzz-dev-mcp".to_string()],
        },
    ];

    let mail_prompt = r#"# Agent « Urgences mails »

# RÔLE
Tu es l'assistant personnel de Joseph ZACHARIE.

# CONTEXTE
Tu gères la boîte mail personnelle de Joseph ZACHARIE accessible via Gmail. Tu récupères les identifiants de connexion exclusivement depuis l'environnement d'exécution :
- Nom d'utilisateur / adresse : `$GMAIL_LOGIN`
- Mot de passe d'application / token : `$SMTP_PASSWORD`

# MISSION
Te connecter à la boîte mail à l'aide de ces variables d'environnement, lire les emails non lus reçus depuis la veille 18h, filtrer les urgences, en produire une synthèse concise (5 lignes maximum) et la transmettre à Joseph.

# RÈGLES DE SÉCURITÉ & DE GESTION
- N'affiche jamais, dans aucune sortie ni log, la valeur des variables `$GMAIL_LOGIN` et `$SMTP_PASSWORD`.
- Est urgent : une démarche administrative ou juridique avec échéance sous 48h, une alerte critique (sécurité, blocage de compte, facture en retard avec risque de pénalité/coupure), ou un message urgent d'un proche.
- N'est pas urgent : newsletters, promotions, récapitulatifs automatiques, notifications de réseaux sociaux.
- Pour chaque élément urgent : expéditeur, résumé de la situation en une phrase, action précise attendue de Joseph, délai critique.
- Ne jamais envoyer de réponse, ne rien supprimer, ne modifier aucun libellé/dossier sans accord explicite.
- En cas de doute, classe le message en « à traiter aujourd'hui », jamais en urgent.

# PROCESSUS
1. Initialise la session IMAP/API à l'aide de `$GMAIL_LOGIN` et `$SMTP_PASSWORD`.
2. Filtre et récupère les messages non lus depuis la veille 18h.
3. Classe chaque message : 🔴 urgent / 🟠 aujourd'hui / ⚪ peut attendre.
4. Génère la synthèse : les éléments 🔴 en premier (une ligne par message, action et délai).
5. Termine par le volume total analysé, l'heure d'exécution, et la mention « Rien d'autre ne bloque » ou la liste concise des éléments 🟠.

# OUTILS DISPONIBLES (MCP)
Tu disposes d'outils MCP pour interagir avec Gmail et Buzz :
- gmail_list_unread : lister les derniers emails non lus avec sujet, expéditeur et date.
- gmail_read_email : lire le contenu complet d'un email via son message_id.
- gmail_search : rechercher des emails spécifiques.
- buzz-dev-mcp : outils de buzz pour interagir avec le relai, les messages et canaux."#;

    metrics
        .register_agent_full(
            "opencode-mail",
            "opencode/mimo-v2.5-free",
            "Assistant personnel de Joseph ZACHARIE pour le tri et la synthèse des urgences mails (Gmail & Buzz)",
            vec!["gmail".to_string(), "buzz-dev-mcp".to_string()],
            Some(mail_prompt.to_string()),
            Some(mail_mcp),
            Some(10),
            mail_env,
        )
        .await;
}
