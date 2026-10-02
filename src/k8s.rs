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
    pub mcp_servers: Vec<McpServerConfig>,
    pub max_iterations: Option<u32>,
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
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(false),
        Err(e) => Err(e),
    }
}

pub async fn get_agent_config(
    client: &kube::Client,
    ns: &str,
    agent_name: &str,
) -> Result<Option<AgentConfigMapData>, kube::Error> {
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), ns);
    match cms.get(agent_name).await {
        Ok(cm) => {
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
            Ok(None)
        }
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(None),
        Err(e) => Err(e),
    }
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
                    let tools: Vec<String> =
                        config.mcp_servers.into_iter().map(|s| s.name).collect();

                    metrics
                        .register_agent(&agent_name, &model, &desc, tools)
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
    let job_id = format!("{}-{}", agent_name, short_uuid);

    // Retrieve agent model if known
    let model = match get_agent_config(client, ns, agent_name).await {
        Ok(Some(cfg)) => cfg
            .model
            .unwrap_or_else(|| "opencode/free-default-model".to_string()),
        _ => "opencode/free-default-model".to_string(),
    };

    let job_manifest: Job = serde_json::from_value(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": job_id,
            "namespace": ns,
            "labels": {
                "app.kubernetes.io/managed-by": "aramaki",
                "agent-name": agent_name
            }
        },
        "spec": {
            "ttlSecondsAfterFinished": 300,
            "template": {
                "spec": {
                    "restartPolicy": "Never",
                    "containers": [{
                        "name": "opencode-agent",
                        "image": agent_runner_image,
                        "env": [
                            { "name": "USER_PROMPT", "value": prompt },
                            { "name": "SLACK_CHANNEL", "value": channel },
                            { "name": "SLACK_THREAD_TS", "value": thread_ts },
                            { "name": "AGENT_CONFIG_PATH", "value": "/etc/agent/agent.json" },
                            { "name": "ARAMAKI_JOB_ID", "value": job_id }
                        ],
                        "volumeMounts": [{
                            "name": "agent-config",
                            "mountPath": "/etc/agent",
                            "readOnly": true
                        }]
                    }],
                    "volumes": [{
                        "name": "agent-config",
                        "configMap": {
                            "name": agent_name
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
    metrics
        .register_agent(
            "agent-code-reviewer",
            "opencode/free-default-model",
            "Analyse le code et propose des optimisations sur le cluster Jo3",
            vec!["filesystem".to_string(), "git".to_string()],
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
}
