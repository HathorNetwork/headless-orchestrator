use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::RwLock;

/// Represents a running wallet-headless instance.
#[derive(Debug, Clone)]
pub struct Instance {
    /// Unique session token (used as both session ID and Docker container name suffix)
    pub session_id: String,
    /// Docker container ID
    pub container_id: String,
    /// The host port mapped to the container's headless port
    pub port: u16,
    /// API key for authenticating with this wallet-headless instance
    pub api_key: String,
    /// When this instance was created
    pub created_at: Instant,
    /// Last time a request was proxied to this instance
    pub last_activity: Instant,
}

/// Shared application state.
pub struct AppState {
    /// Running instances keyed by session_id
    pub instances: RwLock<HashMap<String, Instance>>,
    /// Fullnode URL passed to containers
    pub fullnode_url: String,
    /// Tx-mining URL passed to containers (optional)
    pub tx_mining_url: Option<String>,
    /// Network name
    pub network: String,
    /// Docker image for wallet-headless
    pub headless_image: String,
    /// Docker network to attach to
    pub docker_network: Option<String>,
    /// Max allowed instances
    pub max_instances: usize,
    /// Idle timeout in seconds
    pub idle_timeout_secs: u64,
    /// Host used by the proxy to reach spawned containers (see --proxy-host).
    pub proxy_host: String,
    /// HTTP client for health checks and proxying
    pub http_client: reqwest::Client,
}

impl AppState {
    pub fn new(
        fullnode_url: String,
        tx_mining_url: Option<String>,
        network: String,
        headless_image: String,
        docker_network: Option<String>,
        max_instances: usize,
        idle_timeout_secs: u64,
        proxy_host: String,
    ) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .pool_max_idle_per_host(10)
            .build()
            .expect("Failed to build HTTP client");

        Self {
            instances: RwLock::new(HashMap::new()),
            fullnode_url,
            tx_mining_url,
            network,
            headless_image,
            docker_network,
            max_instances,
            idle_timeout_secs,
            proxy_host,
            http_client,
        }
    }
}

pub type SharedState = Arc<AppState>;
