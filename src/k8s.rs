use kube::Api;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::api::batch::v1::Job;
use serde_json::json;
use tracing::info;

pub async fn check_agent_configmap_exists(client: &kube::Client, ns: &str, agent_name: &str) -> Result<bool, kube::Error> {
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), ns);
    match cms.get(agent_name).await {
        Ok(_) => Ok(true),
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(false),
        Err(e) => Err(e),
    }
}

pub async fn spawn_agent_job(
    client: &kube::Client,
    ns: &str,
    agent_runner_image: &str,
    agent_name: &str,
    prompt: &str,
    channel: &str,
    thread_ts: &str,
) -> Result<(), kube::Error> {
    let jobs: Api<Job> = Api::namespaced(client.clone(), ns);
    let job_id = format!("{}-{}", agent_name, &uuid::Uuid::new_v4().to_string()[..8]);

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
                            { "name": "AGENT_CONFIG_PATH", "value": "/etc/agent/agent.json" }
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
    })).unwrap();

    jobs.create(&Default::default(), &job_manifest).await?;
    info!("Job K8s créé par Aramaki : {}", job_id);

    Ok(())
}
