use clap::Parser;
use std::sync::Arc;
use tracing::info;

mod docker;
mod proxy;
mod routes;
mod state;

use state::AppState;

#[derive(Parser, Debug)]
#[command(name = "headless-orchestrator", about = "Multi-tenant wallet-headless orchestrator for Hathor Network")]
struct Args {
    /// Port to listen on
    #[arg(long, default_value = "8100")]
    port: u16,

    /// Hathor fullnode URL (passed to spawned wallet-headless containers)
    #[arg(long, default_value = "http://host.docker.internal:8080/v1a/")]
    fullnode_url: String,

    /// Tx-mining service URL (passed to spawned wallet-headless containers)
    #[arg(long)]
    tx_mining_url: Option<String>,

    /// Network name for wallet-headless (mainnet, testnet, privatenet)
    #[arg(long, default_value = "privatenet")]
    network: String,

    /// Docker image for wallet-headless
    #[arg(long, default_value = "hathornetwork/hathor-wallet-headless:latest")]
    headless_image: String,

    /// Docker network to attach containers to (optional)
    #[arg(long)]
    docker_network: Option<String>,

    /// Max containers (0 = unlimited)
    #[arg(long, default_value = "100")]
    max_instances: usize,

    /// Idle timeout in seconds — containers with no requests are killed
    #[arg(long, default_value = "1800")]
    idle_timeout_secs: u64,

    /// Host used to reach spawned wallet-headless containers. Defaults to
    /// 127.0.0.1 (orchestrator on host). When the orchestrator itself runs
    /// inside a container but still spawns siblings via the host Docker
    /// daemon, set this to `host.docker.internal` so the proxy can reach
    /// the host-published ports. Ignored when `--docker-network` is set
    /// and containers are reached by name (not yet wired).
    #[arg(long, default_value = "127.0.0.1")]
    proxy_host: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "headless_orchestrator=info".into()),
        )
        .init();

    let args = Args::parse();
    let port = args.port;

    let state = Arc::new(AppState::new(
        args.fullnode_url,
        args.tx_mining_url,
        args.network,
        args.headless_image,
        args.docker_network,
        args.max_instances,
        args.idle_timeout_secs,
        args.proxy_host,
    ));

    // Spawn idle reaper
    let reaper_state = state.clone();
    tokio::spawn(async move {
        docker::idle_reaper(reaper_state).await;
    });

    let app = routes::create_router(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .expect("Failed to bind");

    info!(port, "Headless orchestrator listening");

    axum::serve(listener, app).await.expect("Server error");
}
