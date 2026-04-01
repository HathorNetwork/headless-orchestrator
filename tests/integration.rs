//! Integration tests for headless-orchestrator.
//!
//! These tests require:
//! - Docker running
//! - headless-orchestrator running on ORCHESTRATOR_URL (default: http://localhost:8150)
//! - A Hathor fullnode accessible from Docker containers
//!
//! For full e2e tests (faucet, sends), the fullnode must have a funded wallet
//! (e.g., hathor-forge-cli --start).
//!
//! Run: ORCHESTRATOR_URL=http://localhost:8150 cargo test --test integration -- --test-threads=1

use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

fn orchestrator_url() -> String {
    std::env::var("ORCHESTRATOR_URL").unwrap_or_else(|_| "http://localhost:8150".to_string())
}

fn fullnode_url() -> String {
    std::env::var("FULLNODE_URL")
        .unwrap_or_else(|_| "http://localhost:49080".to_string())
}

fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap()
}

// ============================================================================
// Helper Functions
// ============================================================================

struct Session {
    id: String,
    api_key: String,
}

async fn create_session(c: &Client) -> Session {
    let resp: Value = c
        .post(format!("{}/sessions", orchestrator_url()))
        .send()
        .await
        .expect("Failed to create session")
        .json()
        .await
        .expect("Failed to parse session response");

    Session {
        id: resp["session_id"]
            .as_str()
            .expect("No session_id in response")
            .to_string(),
        api_key: resp["api_key"]
            .as_str()
            .expect("No api_key in response")
            .to_string(),
    }
}

async fn destroy_session(c: &Client, session: &Session) {
    c.delete(format!("{}/sessions/{}", orchestrator_url(), session.id))
        .send()
        .await
        .expect("Failed to destroy session");
}

fn api_url(session: &Session, path: &str) -> String {
    format!(
        "{}/sessions/{}/api{}",
        orchestrator_url(),
        session.id,
        path
    )
}

async fn create_wallet(c: &Client, session: &Session, wallet_id: &str, seed: &str) {
    let resp: Value = c
        .post(api_url(session, "/start"))
        .json(&json!({
            "wallet-id": wallet_id,
            "seed": seed,
        }))
        .send()
        .await
        .expect("Failed to create wallet")
        .json()
        .await
        .expect("Failed to parse wallet creation response");

    assert!(
        resp["success"].as_bool().unwrap_or(false),
        "Wallet creation failed: {:?}",
        resp
    );
}

async fn wait_wallet_ready(c: &Client, session: &Session, wallet_id: &str) {
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(2)).await;

        let resp: Value = c
            .get(api_url(session, "/wallet/status"))
            .header("X-Wallet-Id", wallet_id)
            .send()
            .await
            .expect("Failed to get wallet status")
            .json()
            .await
            .expect("Failed to parse wallet status");

        if resp["statusCode"].as_i64() == Some(3) {
            return;
        }
    }
    panic!("Wallet {} did not become ready within 60s", wallet_id);
}

async fn get_first_address(c: &Client, session: &Session, wallet_id: &str) -> String {
    let resp: Value = c
        .get(api_url(session, "/wallet/addresses"))
        .header("X-Wallet-Id", wallet_id)
        .send()
        .await
        .expect("Failed to get addresses")
        .json()
        .await
        .expect("Failed to parse addresses");

    resp["addresses"]
        .as_array()
        .expect("No addresses array")
        .first()
        .expect("No addresses returned")
        .as_str()
        .expect("Address is not a string")
        .to_string()
}

async fn get_balance(c: &Client, session: &Session, wallet_id: &str) -> (i64, i64) {
    get_token_balance(c, session, wallet_id, None).await
}

async fn get_token_balance(
    c: &Client,
    session: &Session,
    wallet_id: &str,
    token: Option<&str>,
) -> (i64, i64) {
    let mut url = api_url(session, "/wallet/balance");
    if let Some(token_uid) = token {
        url = format!("{}?token={}", url, token_uid);
    }

    let resp: Value = c
        .get(&url)
        .header("X-Wallet-Id", wallet_id)
        .send()
        .await
        .expect("Failed to get balance")
        .json()
        .await
        .expect("Failed to parse balance");

    let available = resp["available"].as_i64().unwrap_or(0);
    let locked = resp["locked"].as_i64().unwrap_or(0);
    (available, locked)
}

async fn send_from_faucet(c: &Client, address: &str, amount_cents: i64) -> Value {
    c.post(format!("{}/v1a/wallet/send_tokens/", fullnode_url()))
        .json(&json!({
            "data": {
                "inputs": [],
                "outputs": [{
                    "address": address,
                    "value": amount_cents,
                }]
            }
        }))
        .send()
        .await
        .expect("Failed to send from faucet")
        .json()
        .await
        .expect("Failed to parse faucet send response")
}

async fn send_tx(
    c: &Client,
    session: &Session,
    wallet_id: &str,
    address: &str,
    value: i64,
) -> Value {
    c.post(api_url(session, "/wallet/simple-send-tx"))
        .header("X-Wallet-Id", wallet_id)
        .json(&json!({
            "address": address,
            "value": value,
        }))
        .send()
        .await
        .expect("Failed to send transaction")
        .json()
        .await
        .expect("Failed to parse send response")
}

// ============================================================================
// Seeds (different for each wallet to get unique addresses)
// ============================================================================

const SEED_ALICE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";
const SEED_BOB: &str = "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo vote";

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
async fn test_health() {
    let c = client();
    let resp = c
        .get(format!("{}/health", orchestrator_url()))
        .send()
        .await
        .expect("Health check failed");

    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_session_lifecycle() {
    let c = client();

    // Create
    let session = create_session(&c).await;
    assert!(!session.id.is_empty());
    assert!(!session.api_key.is_empty());
    println!("Created session: {} (api_key: {})", session.id, session.api_key);

    // List — should contain our session
    let resp: Value = c
        .get(format!("{}/sessions", orchestrator_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let sessions = resp["sessions"].as_array().unwrap();
    assert!(
        sessions
            .iter()
            .any(|s| s["session_id"].as_str() == Some(&session.id)),
        "Session not found in list"
    );

    // Destroy
    destroy_session(&c, &session).await;

    // List — should not contain our session
    let resp: Value = c
        .get(format!("{}/sessions", orchestrator_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let sessions = resp["sessions"].as_array().unwrap();
    assert!(
        !sessions
            .iter()
            .any(|s| s["session_id"].as_str() == Some(&session.id)),
        "Session should have been destroyed"
    );
}

#[tokio::test]
async fn test_wallet_creation_and_sync() {
    let c = client();
    let session = create_session(&c).await;

    // Create wallet
    create_wallet(&c, &session, "alice", SEED_ALICE).await;

    // Wait for sync
    wait_wallet_ready(&c, &session, "alice").await;

    // Get addresses
    let addr = get_first_address(&c, &session, "alice").await;
    // Mainnet addresses start with H, privatenet/testnet with W
    assert!(
        addr.starts_with('H') || addr.starts_with('W'),
        "Address should start with H or W, got {}",
        addr
    );
    println!("Alice first address: {}", addr);

    // Get balance
    let (available, locked) = get_balance(&c, &session, "alice").await;
    println!("Alice balance: available={}, locked={}", available, locked);

    // Cleanup
    destroy_session(&c, &session).await;
}

#[tokio::test]
async fn test_session_isolation() {
    let c = client();

    // Create two sessions
    let session1 = create_session(&c).await;
    let session2 = create_session(&c).await;

    // Create wallets with same wallet-id but different seeds
    create_wallet(&c, &session1, "wallet", SEED_ALICE).await;
    create_wallet(&c, &session2, "wallet", SEED_BOB).await;

    // Wait for both to sync
    wait_wallet_ready(&c, &session1, "wallet").await;
    wait_wallet_ready(&c, &session2, "wallet").await;

    // Get addresses — should be different because seeds are different
    let addr1 = get_first_address(&c, &session1, "wallet").await;
    let addr2 = get_first_address(&c, &session2, "wallet").await;

    assert_ne!(
        addr1, addr2,
        "Same wallet-id on different sessions should have different addresses"
    );

    println!("Session 1 wallet address: {}", addr1);
    println!("Session 2 wallet address: {}", addr2);

    // Cleanup
    destroy_session(&c, &session1).await;
    destroy_session(&c, &session2).await;
}
