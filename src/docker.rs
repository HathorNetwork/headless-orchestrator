use bollard::container::{
    Config, CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
};
use bollard::models::{EndpointSettings, HostConfig, PortBinding};
use bollard::Docker;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::state::{Instance, SharedState};

/// Find a free port by binding to :0 and reading the assigned port.
async fn find_free_port() -> Result<u16, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to find free port: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get local addr: {}", e))?
        .port();
    drop(listener);
    Ok(port)
}

/// Spawn a new wallet-headless Docker container for a session.
pub async fn spawn_instance(state: &SharedState, session_id: &str) -> Result<Instance, String> {
    let docker =
        Docker::connect_with_local_defaults().map_err(|e| format!("Docker connect: {}", e))?;

    let host_port = find_free_port().await?;
    let container_name = format!("headless-{}", session_id);
    let container_port = "8000";
    let api_key = Uuid::new_v4().to_string();

    let mut env = vec![
        "HEADLESS_HTTP_BIND=0.0.0.0".to_string(),
        format!("HEADLESS_HTTP_PORT={}", container_port),
        format!("HEADLESS_NETWORK={}", state.network),
        format!("HEADLESS_SERVER={}", state.fullnode_url),
        format!("HEADLESS_API_KEY={}", api_key),
        // Dummy seed required by startup validation — actual wallets are created via POST /start
        "HEADLESS_SEED_DEFAULT=abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".to_string(),
    ];

    if let Some(ref tx_url) = state.tx_mining_url {
        env.push(format!("HEADLESS_TX_MINING_URL={}", tx_url));
    }

    let mut port_bindings = HashMap::new();
    port_bindings.insert(
        format!("{}/tcp", container_port),
        Some(vec![PortBinding {
            host_ip: Some("127.0.0.1".to_string()),
            host_port: Some(host_port.to_string()),
        }]),
    );

    let mut host_config = HostConfig {
        port_bindings: Some(port_bindings),
        // Allow container to reach host services
        extra_hosts: Some(vec!["host.docker.internal:host-gateway".to_string()]),
        ..Default::default()
    };

    // Networking config for custom Docker network
    let mut networking_config = None;
    if let Some(ref network) = state.docker_network {
        host_config.network_mode = Some(network.clone());
        let mut endpoints = HashMap::new();
        endpoints.insert(
            network.clone(),
            EndpointSettings {
                ..Default::default()
            },
        );
        networking_config = Some(bollard::container::NetworkingConfig {
            endpoints_config: endpoints,
        });
    }

    let config = Config {
        image: Some(state.headless_image.clone()),
        // Bypass LavaMoat entrypoint which is broken in the latest Docker image
        entrypoint: Some(vec!["node".to_string(), "dist/index.js".to_string()]),
        env: Some(env),
        exposed_ports: Some({
            let mut ports = HashMap::new();
            ports.insert(
                format!("{}/tcp", container_port),
                HashMap::<(), ()>::new(),
            );
            ports
        }),
        host_config: Some(host_config),
        networking_config,
        labels: Some({
            let mut labels = HashMap::new();
            labels.insert("managed-by".to_string(), "headless-orchestrator".to_string());
            labels.insert("session-id".to_string(), session_id.to_string());
            labels
        }),
        ..Default::default()
    };

    docker
        .create_container(
            Some(CreateContainerOptions {
                name: &container_name,
                platform: None,
            }),
            config,
        )
        .await
        .map_err(|e| format!("Failed to create container: {}", e))?;

    docker
        .start_container(&container_name, None::<StartContainerOptions<String>>)
        .await
        .map_err(|e| format!("Failed to start container: {}", e))?;

    // Wait for the container to be ready by probing the wallet status endpoint
    // (wallet-headless doesn't have a /health endpoint)
    let headless_url = format!("http://127.0.0.1:{}", host_port);
    let client = &state.http_client;
    let mut ready = false;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Ok(resp) = client
            .get(format!("{}/wallet/status", headless_url))
            .header("X-Wallet-Id", "probe")
            .header("x-api-key", &api_key)
            .send()
            .await
        {
            // Any HTTP response means the server is up (even 4xx)
            if resp.status().as_u16() > 0 {
                ready = true;
                break;
            }
        }
    }

    if !ready {
        // Clean up failed container
        let _ = docker
            .remove_container(
                &container_name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        return Err("Container started but wallet-headless didn't become healthy within 30s".to_string());
    }

    let now = Instant::now();
    let container_id = container_name.clone();
    let instance = Instance {
        session_id: session_id.to_string(),
        container_id,
        port: host_port,
        api_key,
        created_at: now,
        last_activity: now,
    };

    info!(
        session_id,
        port = host_port,
        container = container_name,
        "Spawned wallet-headless instance"
    );

    Ok(instance)
}

/// Remove a Docker container by name.
pub async fn remove_instance(session_id: &str) -> Result<(), String> {
    let docker =
        Docker::connect_with_local_defaults().map_err(|e| format!("Docker connect: {}", e))?;

    let container_name = format!("headless-{}", session_id);

    docker
        .remove_container(
            &container_name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("Failed to remove container {}: {}", container_name, e))?;

    info!(session_id, "Removed wallet-headless container");
    Ok(())
}

/// Background task that periodically reaps idle instances.
pub async fn idle_reaper(state: SharedState) {
    let interval = Duration::from_secs(60);
    let timeout = Duration::from_secs(state.idle_timeout_secs);

    loop {
        tokio::time::sleep(interval).await;

        let expired: Vec<String> = {
            let instances = state.instances.read().await;
            instances
                .iter()
                .filter(|(_, inst)| inst.last_activity.elapsed() > timeout)
                .map(|(id, _)| id.clone())
                .collect()
        };

        for session_id in expired {
            warn!(session_id, "Reaping idle instance");
            {
                let mut instances = state.instances.write().await;
                instances.remove(&session_id);
            }
            if let Err(e) = remove_instance(&session_id).await {
                error!(session_id, error = %e, "Failed to remove idle container");
            }
        }
    }
}
