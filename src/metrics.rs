use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionStatus {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub id: String,
    pub agent_name: String,
    pub model: String,
    pub channel: String,
    pub prompt_preview: String,
    pub status: ExecutionStatus,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub duration_secs: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStats {
    pub name: String,
    pub model: String,
    pub description: String,
    pub tools_exposed: Vec<String>,
    pub total_runs: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub running_count: u64,
    pub total_duration_secs: f64,
    pub last_run: Option<DateTime<Utc>>,
}

impl AgentStats {
    pub fn new(
        name: String,
        model: String,
        description: String,
        tools_exposed: Vec<String>,
    ) -> Self {
        Self {
            name,
            model,
            description,
            tools_exposed,
            total_runs: 0,
            success_count: 0,
            failure_count: 0,
            running_count: 0,
            total_duration_secs: 0.0,
            last_run: None,
        }
    }

    pub fn average_duration_secs(&self) -> f64 {
        let completed = self.success_count + self.failure_count;
        if completed == 0 {
            0.0
        } else {
            self.total_duration_secs / completed as f64
        }
    }

    pub fn success_rate(&self) -> f64 {
        let completed = self.success_count + self.failure_count;
        if completed == 0 {
            100.0
        } else {
            (self.success_count as f64 / completed as f64) * 100.0
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStats {
    pub model: String,
    pub invocations: u64,
    pub success_count: u64,
    pub failure_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStats {
    pub tool_name: String,
    pub agents: Vec<String>,
    pub invocations: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardSummary {
    pub status: ConnectionStatus,
    pub uptime_seconds: u64,
    pub uptime_formatted: String,
    pub requests_total: u64,
    pub requests_success: u64,
    pub requests_failed: u64,
    pub agents_spawned_total: u64,
    pub agents_running: u64,
    pub agents_succeeded: u64,
    pub agents_failed: u64,
    pub global_success_rate: f64,
    pub agents: Vec<AgentStats>,
    pub models: Vec<ModelStats>,
    pub tools: Vec<ToolStats>,
    pub recent_executions: Vec<ExecutionRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionStatus {
    pub k8s_connected: bool,
    pub namespace: String,
    pub slack_active: bool,
    pub mode: String,
}

pub struct MetricsStore {
    start_time: DateTime<Utc>,
    k8s_connected: AtomicBool,
    requests_total: AtomicU64,
    requests_success: AtomicU64,
    requests_failed: AtomicU64,
    agents_spawned_total: AtomicU64,
    agent_stats: Arc<RwLock<HashMap<String, AgentStats>>>,
    model_stats: Arc<RwLock<HashMap<String, ModelStats>>>,
    tool_stats: Arc<RwLock<HashMap<String, ToolStats>>>,
    recent_executions: Arc<RwLock<VecDeque<ExecutionRecord>>>,
}

impl MetricsStore {
    pub fn new() -> Self {
        Self {
            start_time: Utc::now(),
            k8s_connected: AtomicBool::new(false),
            requests_total: AtomicU64::new(0),
            requests_success: AtomicU64::new(0),
            requests_failed: AtomicU64::new(0),
            agents_spawned_total: AtomicU64::new(0),
            agent_stats: Arc::new(RwLock::new(HashMap::new())),
            model_stats: Arc::new(RwLock::new(HashMap::new())),
            tool_stats: Arc::new(RwLock::new(HashMap::new())),
            recent_executions: Arc::new(RwLock::new(VecDeque::with_capacity(50))),
        }
    }

    pub fn set_k8s_connected(&self, connected: bool) {
        self.k8s_connected.store(connected, Ordering::SeqCst);
    }

    pub fn is_k8s_connected(&self) -> bool {
        self.k8s_connected.load(Ordering::SeqCst)
    }

    pub fn record_request(&self, success: bool) {
        self.requests_total.fetch_add(1, Ordering::SeqCst);
        if success {
            self.requests_success.fetch_add(1, Ordering::SeqCst);
        } else {
            self.requests_failed.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub async fn register_agent(
        &self,
        name: &str,
        model: &str,
        description: &str,
        tools: Vec<String>,
    ) {
        let mut agents = self.agent_stats.write().await;
        let entry = agents.entry(name.to_string()).or_insert_with(|| {
            AgentStats::new(
                name.to_string(),
                model.to_string(),
                description.to_string(),
                tools.clone(),
            )
        });

        entry.model = model.to_string();
        if !description.is_empty() {
            entry.description = description.to_string();
        }
        for tool in &tools {
            if !entry.tools_exposed.contains(tool) {
                entry.tools_exposed.push(tool.clone());
            }
        }

        // Register tools
        let mut tools_map = self.tool_stats.write().await;
        for tool in tools {
            let t_entry = tools_map.entry(tool.clone()).or_insert_with(|| ToolStats {
                tool_name: tool,
                agents: Vec::new(),
                invocations: 0,
            });
            if !t_entry.agents.contains(&name.to_string()) {
                t_entry.agents.push(name.to_string());
            }
        }
    }

    pub async fn record_agent_spawn(
        &self,
        job_id: &str,
        agent_name: &str,
        model: &str,
        channel: &str,
        prompt: &str,
    ) {
        self.agents_spawned_total.fetch_add(1, Ordering::SeqCst);
        let now = Utc::now();

        // Update agent stats
        {
            let mut agents = self.agent_stats.write().await;
            let entry = agents.entry(agent_name.to_string()).or_insert_with(|| {
                AgentStats::new(
                    agent_name.to_string(),
                    model.to_string(),
                    "Dynamic Section 9 Agent".to_string(),
                    Vec::new(),
                )
            });
            entry.total_runs += 1;
            entry.running_count += 1;
            entry.last_run = Some(now);
            if entry.model.is_empty() || entry.model == "unknown" {
                entry.model = model.to_string();
            }
        }

        // Update model stats
        {
            let mut models = self.model_stats.write().await;
            let entry = models
                .entry(model.to_string())
                .or_insert_with(|| ModelStats {
                    model: model.to_string(),
                    invocations: 0,
                    success_count: 0,
                    failure_count: 0,
                });
            entry.invocations += 1;
        }

        // Add to recent executions
        {
            let mut recents = self.recent_executions.write().await;
            if recents.len() >= 50 {
                recents.pop_back();
            }
            let prompt_preview = if prompt.len() > 100 {
                format!("{}...", &prompt[..97])
            } else {
                prompt.to_string()
            };
            recents.push_front(ExecutionRecord {
                id: job_id.to_string(),
                agent_name: agent_name.to_string(),
                model: model.to_string(),
                channel: channel.to_string(),
                prompt_preview,
                status: ExecutionStatus::Running,
                started_at: now,
                completed_at: None,
                duration_secs: None,
            });
        }
    }

    pub async fn record_agent_completion(
        &self,
        job_id: &str,
        success: bool,
        duration_secs: Option<f64>,
    ) {
        let now = Utc::now();
        let mut target_agent = String::new();
        let mut target_model = String::new();
        let mut computed_duration = duration_secs;

        // Update in recent executions
        {
            let mut recents = self.recent_executions.write().await;
            for exec in recents.iter_mut() {
                if exec.id == job_id {
                    if exec.status != ExecutionStatus::Running {
                        // Already completed, don't double count
                        return;
                    }
                    exec.status = if success {
                        ExecutionStatus::Succeeded
                    } else {
                        ExecutionStatus::Failed
                    };
                    exec.completed_at = Some(now);
                    let dur = computed_duration.unwrap_or_else(|| {
                        let diff = now.signed_duration_since(exec.started_at);
                        diff.num_milliseconds() as f64 / 1000.0
                    });
                    exec.duration_secs = Some(dur);
                    computed_duration = Some(dur);
                    target_agent = exec.agent_name.clone();
                    target_model = exec.model.clone();
                    break;
                }
            }
        }

        if target_agent.is_empty() {
            return;
        }

        let dur = computed_duration.unwrap_or(0.0);

        // Update Agent stats
        {
            let mut agents = self.agent_stats.write().await;
            if let Some(entry) = agents.get_mut(&target_agent) {
                if entry.running_count > 0 {
                    entry.running_count -= 1;
                }
                if success {
                    entry.success_count += 1;
                } else {
                    entry.failure_count += 1;
                }
                entry.total_duration_secs += dur;
            }
        }

        // Update Model stats
        {
            let mut models = self.model_stats.write().await;
            if let Some(entry) = models.get_mut(&target_model) {
                if success {
                    entry.success_count += 1;
                } else {
                    entry.failure_count += 1;
                }
            }
        }
    }

    pub async fn record_tool_invocation(&self, tool_name: &str) {
        let mut tools = self.tool_stats.write().await;
        if let Some(tool) = tools.get_mut(tool_name) {
            tool.invocations += 1;
        }
    }

    pub async fn get_dashboard_summary(&self, namespace: &str) -> DashboardSummary {
        let uptime = Utc::now().signed_duration_since(self.start_time);
        let uptime_secs = uptime.num_seconds().max(0) as u64;
        let days = uptime_secs / 86400;
        let hours = (uptime_secs % 86400) / 3600;
        let mins = (uptime_secs % 3600) / 60;
        let secs = uptime_secs % 60;
        let uptime_formatted = if days > 0 {
            format!("{}d {}h {}m", days, hours, mins)
        } else if hours > 0 {
            format!("{}h {}m {}s", hours, mins, secs)
        } else {
            format!("{}m {}s", mins, secs)
        };

        let agents = self.agent_stats.read().await;
        let mut agents_list: Vec<AgentStats> = agents.values().cloned().collect();
        agents_list.sort_by_key(|b| std::cmp::Reverse(b.total_runs));

        let models = self.model_stats.read().await;
        let mut models_list: Vec<ModelStats> = models.values().cloned().collect();
        models_list.sort_by_key(|b| std::cmp::Reverse(b.invocations));

        let tools = self.tool_stats.read().await;
        let mut tools_list: Vec<ToolStats> = tools.values().cloned().collect();
        tools_list.sort_by_key(|b| std::cmp::Reverse(b.invocations));

        let recents = self.recent_executions.read().await;
        let recent_executions: Vec<ExecutionRecord> = recents.iter().cloned().collect();

        let mut total_succeeded = 0u64;
        let mut total_failed = 0u64;
        let mut total_running = 0u64;
        for agent in &agents_list {
            total_succeeded += agent.success_count;
            total_failed += agent.failure_count;
            total_running += agent.running_count;
        }

        let completed = total_succeeded + total_failed;
        let global_success_rate = if completed == 0 {
            100.0
        } else {
            (total_succeeded as f64 / completed as f64) * 100.0
        };

        let is_connected = self.is_k8s_connected();
        DashboardSummary {
            status: ConnectionStatus {
                k8s_connected: is_connected,
                namespace: namespace.to_string(),
                slack_active: true,
                mode: if is_connected {
                    "Kubernetes Cluster Active".to_string()
                } else {
                    "Standalone / Simulated".to_string()
                },
            },
            uptime_seconds: uptime_secs,
            uptime_formatted,
            requests_total: self.requests_total.load(Ordering::SeqCst),
            requests_success: self.requests_success.load(Ordering::SeqCst),
            requests_failed: self.requests_failed.load(Ordering::SeqCst),
            agents_spawned_total: self.agents_spawned_total.load(Ordering::SeqCst),
            agents_running: total_running,
            agents_succeeded: total_succeeded,
            agents_failed: total_failed,
            global_success_rate,
            agents: agents_list,
            models: models_list,
            tools: tools_list,
            recent_executions,
        }
    }

    pub async fn to_prometheus_text(&self, namespace: &str) -> String {
        let mut out = String::with_capacity(4096);
        let summary = self.get_dashboard_summary(namespace).await;

        out.push_str("# HELP aramaki_connected Indicates if Aramaki is connected to Kubernetes and operational (1 = connected, 0 = disconnected)\n");
        out.push_str("# TYPE aramaki_connected gauge\n");
        out.push_str(&format!(
            "aramaki_connected{{service=\"aramaki\",namespace=\"{}\"}} {}\n\n",
            namespace,
            if summary.status.k8s_connected { 1 } else { 0 }
        ));

        out.push_str("# HELP aramaki_uptime_seconds Process uptime in seconds\n");
        out.push_str("# TYPE aramaki_uptime_seconds gauge\n");
        out.push_str(&format!(
            "aramaki_uptime_seconds {}\n\n",
            summary.uptime_seconds
        ));

        out.push_str("# HELP aramaki_requests_total Total number of incoming requests received\n");
        out.push_str("# TYPE aramaki_requests_total counter\n");
        out.push_str(&format!(
            "aramaki_requests_total{{status=\"success\"}} {}\n",
            summary.requests_success
        ));
        out.push_str(&format!(
            "aramaki_requests_total{{status=\"failed\"}} {}\n\n",
            summary.requests_failed
        ));

        out.push_str("# HELP aramaki_agents_spawned_total Total number of agent jobs spawned\n");
        out.push_str("# TYPE aramaki_agents_spawned_total counter\n");
        for agent in &summary.agents {
            out.push_str(&format!(
                "aramaki_agents_spawned_total{{agent=\"{}\",model=\"{}\"}} {}\n",
                agent.name, agent.model, agent.total_runs
            ));
        }
        out.push('\n');

        out.push_str("# HELP aramaki_agent_executions_total Agent job executions by status\n");
        out.push_str("# TYPE aramaki_agent_executions_total counter\n");
        for agent in &summary.agents {
            out.push_str(&format!(
                "aramaki_agent_executions_total{{agent=\"{}\",model=\"{}\",status=\"succeeded\"}} {}\n",
                agent.name, agent.model, agent.success_count
            ));
            out.push_str(&format!(
                "aramaki_agent_executions_total{{agent=\"{}\",model=\"{}\",status=\"failed\"}} {}\n",
                agent.name, agent.model, agent.failure_count
            ));
            out.push_str(&format!(
                "aramaki_agent_executions_total{{agent=\"{}\",model=\"{}\",status=\"running\"}} {}\n",
                agent.name, agent.model, agent.running_count
            ));
        }
        out.push('\n');

        out.push_str("# HELP aramaki_agent_duration_seconds Average duration of agent executions in seconds\n");
        out.push_str("# TYPE aramaki_agent_duration_seconds gauge\n");
        for agent in &summary.agents {
            out.push_str(&format!(
                "aramaki_agent_duration_seconds{{agent=\"{}\"}} {:.3}\n",
                agent.name,
                agent.average_duration_secs()
            ));
        }
        out.push('\n');

        out.push_str("# HELP aramaki_models_consumed_total Total times a model has been invoked\n");
        out.push_str("# TYPE aramaki_models_consumed_total counter\n");
        for model in &summary.models {
            out.push_str(&format!(
                "aramaki_models_consumed_total{{model=\"{}\"}} {}\n",
                model.model, model.invocations
            ));
            out.push_str(&format!(
                "aramaki_model_success_total{{model=\"{}\"}} {}\n",
                model.model, model.success_count
            ));
            out.push_str(&format!(
                "aramaki_model_failure_total{{model=\"{}\"}} {}\n",
                model.model, model.failure_count
            ));
        }
        out.push('\n');

        out.push_str("# HELP aramaki_tools_exposed Number of MCP tools exposed to an agent\n");
        out.push_str("# TYPE aramaki_tools_exposed gauge\n");
        for agent in &summary.agents {
            for tool in &agent.tools_exposed {
                out.push_str(&format!(
                    "aramaki_tools_exposed{{agent=\"{}\",tool=\"{}\"}} 1\n",
                    agent.name, tool
                ));
            }
        }
        out.push('\n');

        out.push_str("# HELP aramaki_tool_calls_total Total tool calls/invocations recorded\n");
        out.push_str("# TYPE aramaki_tool_calls_total counter\n");
        for tool in &summary.tools {
            out.push_str(&format!(
                "aramaki_tool_calls_total{{tool=\"{}\"}} {}\n",
                tool.tool_name, tool.invocations
            ));
        }
        out.push('\n');

        out
    }

    pub async fn to_opentelemetry_json(&self, namespace: &str) -> serde_json::Value {
        let summary = self.get_dashboard_summary(namespace).await;
        let now_unix_nano = Utc::now().timestamp_nanos_opt().unwrap_or(0);

        let mut metrics_list = Vec::new();

        // 1. Connection gauge
        metrics_list.push(serde_json::json!({
            "name": "aramaki.connected",
            "description": "Indicates if Aramaki is connected to Kubernetes and operational",
            "unit": "1",
            "gauge": {
                "dataPoints": [{
                    "timeUnixNano": now_unix_nano.to_string(),
                    "asInt": if summary.status.k8s_connected { 1 } else { 0 },
                    "attributes": [
                        { "key": "service", "value": { "stringValue": "aramaki" } },
                        { "key": "namespace", "value": { "stringValue": namespace } }
                    ]
                }]
            }
        }));

        // 2. Requests counter
        metrics_list.push(serde_json::json!({
            "name": "aramaki.requests.total",
            "description": "Total number of incoming requests received",
            "unit": "1",
            "sum": {
                "aggregationTemporality": 2, // Cumulative
                "isMonotonic": true,
                "dataPoints": [
                    {
                        "timeUnixNano": now_unix_nano.to_string(),
                        "asInt": summary.requests_success,
                        "attributes": [{ "key": "status", "value": { "stringValue": "success" } }]
                    },
                    {
                        "timeUnixNano": now_unix_nano.to_string(),
                        "asInt": summary.requests_failed,
                        "attributes": [{ "key": "status", "value": { "stringValue": "failed" } }]
                    }
                ]
            }
        }));

        // 3. Agent executions
        let mut agent_points = Vec::new();
        for agent in &summary.agents {
            agent_points.push(serde_json::json!({
                "timeUnixNano": now_unix_nano.to_string(),
                "asInt": agent.success_count,
                "attributes": [
                    { "key": "agent", "value": { "stringValue": agent.name } },
                    { "key": "model", "value": { "stringValue": agent.model } },
                    { "key": "status", "value": { "stringValue": "succeeded" } }
                ]
            }));
            agent_points.push(serde_json::json!({
                "timeUnixNano": now_unix_nano.to_string(),
                "asInt": agent.failure_count,
                "attributes": [
                    { "key": "agent", "value": { "stringValue": agent.name } },
                    { "key": "model", "value": { "stringValue": agent.model } },
                    { "key": "status", "value": { "stringValue": "failed" } }
                ]
            }));
        }

        metrics_list.push(serde_json::json!({
            "name": "aramaki.agent.executions",
            "description": "Agent job executions outcome",
            "unit": "1",
            "sum": {
                "aggregationTemporality": 2,
                "isMonotonic": true,
                "dataPoints": agent_points
            }
        }));

        // 4. Models consumed
        let mut model_points = Vec::new();
        for model in &summary.models {
            model_points.push(serde_json::json!({
                "timeUnixNano": now_unix_nano.to_string(),
                "asInt": model.invocations,
                "attributes": [
                    { "key": "model", "value": { "stringValue": model.model } }
                ]
            }));
        }

        metrics_list.push(serde_json::json!({
            "name": "aramaki.models.consumed",
            "description": "Total times an AI model has been consumed",
            "unit": "1",
            "sum": {
                "aggregationTemporality": 2,
                "isMonotonic": true,
                "dataPoints": model_points
            }
        }));

        serde_json::json!({
            "resourceMetrics": [{
                "resource": {
                    "attributes": [
                        { "key": "service.name", "value": { "stringValue": "aramaki" } },
                        { "key": "service.version", "value": { "stringValue": "0.1.0" } },
                        { "key": "k8s.namespace.name", "value": { "stringValue": namespace } }
                    ]
                },
                "scopeMetrics": [{
                    "scope": {
                        "name": "aramaki.orchestrator",
                        "version": "0.1.0"
                    },
                    "metrics": metrics_list
                }]
            }]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_metrics_store_flow() {
        let store = MetricsStore::new();
        store.set_k8s_connected(true);
        assert!(store.is_k8s_connected());

        store.record_request(true);
        store.record_request(false);

        store
            .register_agent(
                "agent-code-reviewer",
                "opencode/free-default-model",
                "Test description",
                vec!["filesystem".to_string(), "git".to_string()],
            )
            .await;

        store
            .record_agent_spawn(
                "job-1",
                "agent-code-reviewer",
                "opencode/free-default-model",
                "#dev",
                "Review PR #42",
            )
            .await;

        store
            .record_agent_completion("job-1", true, Some(4.5))
            .await;
        store.record_tool_invocation("git").await;

        let summary = store.get_dashboard_summary("aramaki").await;
        assert_eq!(summary.requests_total, 2);
        assert_eq!(summary.requests_success, 1);
        assert_eq!(summary.requests_failed, 1);
        assert_eq!(summary.agents_spawned_total, 1);
        assert_eq!(summary.agents_succeeded, 1);
        assert_eq!(summary.global_success_rate, 100.0);
        assert_eq!(summary.agents.len(), 1);
        assert_eq!(summary.agents[0].name, "agent-code-reviewer");
        assert_eq!(summary.agents[0].tools_exposed, vec!["filesystem", "git"]);

        // Verify Prometheus output
        let prom = store.to_prometheus_text("aramaki").await;
        assert!(prom.contains("aramaki_connected{service=\"aramaki\",namespace=\"aramaki\"} 1"));
        assert!(prom.contains("aramaki_requests_total{status=\"success\"} 1"));
        assert!(prom.contains("aramaki_agents_spawned_total{agent=\"agent-code-reviewer\",model=\"opencode/free-default-model\"} 1"));
        assert!(prom.contains(
            "aramaki_tools_exposed{agent=\"agent-code-reviewer\",tool=\"filesystem\"} 1"
        ));
        assert!(prom.contains("aramaki_tool_calls_total{tool=\"git\"} 1"));

        // Verify OpenTelemetry JSON output
        let otel = store.to_opentelemetry_json("aramaki").await;
        assert!(otel.to_string().contains("aramaki.orchestrator"));
        assert!(otel.to_string().contains("aramaki.agent.executions"));
    }
}
