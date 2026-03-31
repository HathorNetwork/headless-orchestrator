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

/// Verify that each container is protected by its own API key:
/// - Direct access without key is rejected
/// - Direct access with wrong key is rejected
/// - Proxy access (which injects the correct key) works
#[tokio::test]
async fn test_api_key_enforcement() {
    let c = client();
    let session = create_session(&c).await;
    println!("Session: {} (api_key: {})", session.id, session.api_key);

    // Get the container's direct port from the list endpoint
    let resp: Value = c
        .get(format!("{}/sessions", orchestrator_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let sessions = resp["sessions"].as_array().unwrap();
    let our_session = sessions
        .iter()
        .find(|s| s["session_id"].as_str() == Some(&session.id))
        .expect("Session not found in list");
    let port = our_session["port"].as_u64().expect("No port in session");
    let direct_url = format!("http://127.0.0.1:{}", port);
    println!("Direct container URL: {}", direct_url);

    // 1. No API key — should be rejected (401)
    let resp_no_key = c
        .get(format!("{}/wallet/status", direct_url))
        .header("X-Wallet-Id", "probe")
        .send()
        .await
        .expect("Request failed");
    let status_no_key = resp_no_key.status().as_u16();
    println!("No API key: HTTP {}", status_no_key);
    assert!(
        status_no_key == 401 || status_no_key == 403,
        "Direct access without API key should be rejected, got {}",
        status_no_key
    );

    // 2. Wrong API key — should be rejected (401)
    let resp_wrong_key = c
        .get(format!("{}/wallet/status", direct_url))
        .header("X-Wallet-Id", "probe")
        .header("x-api-key", "wrong-key-12345")
        .send()
        .await
        .expect("Request failed");
    let status_wrong_key = resp_wrong_key.status().as_u16();
    println!("Wrong API key: HTTP {}", status_wrong_key);
    assert!(
        status_wrong_key == 401 || status_wrong_key == 403,
        "Direct access with wrong API key should be rejected, got {}",
        status_wrong_key
    );

    // 3. Correct API key — should work
    let resp_correct_key = c
        .get(format!("{}/wallet/status", direct_url))
        .header("X-Wallet-Id", "probe")
        .header("x-api-key", &session.api_key)
        .send()
        .await
        .expect("Request failed");
    let status_correct = resp_correct_key.status().as_u16();
    println!("Correct API key: HTTP {}", status_correct);
    assert!(
        status_correct != 401 && status_correct != 403,
        "Direct access with correct API key should not be rejected, got {}",
        status_correct
    );

    // 4. Through proxy (no key in request, proxy injects it) — should work
    let resp_proxy = c
        .get(api_url(&session, "/wallet/status"))
        .header("X-Wallet-Id", "probe")
        .send()
        .await
        .expect("Request failed");
    let status_proxy = resp_proxy.status().as_u16();
    println!("Through proxy: HTTP {}", status_proxy);
    assert!(
        status_proxy != 401 && status_proxy != 403,
        "Proxy should inject the correct API key, got {}",
        status_proxy
    );

    println!("API key enforcement verified");

    // Cleanup
    destroy_session(&c, &session).await;
}

fn tx_hash(resp: &Value) -> &str {
    // Faucet response: { "tx": { "hash": "..." } }
    // Headless response: { "hash": "..." }
    resp.get("tx")
        .and_then(|tx| tx.get("hash"))
        .or_else(|| resp.get("hash"))
        .and_then(|v| v.as_str())
        .unwrap_or("UNKNOWN")
}

fn tx_success(resp: &Value) -> bool {
    resp.get("success").and_then(|v| v.as_bool()).unwrap_or(false)
}

fn format_htr(cents: i64) -> String {
    format!("{}.{:02} HTR", cents / 100, cents % 100)
}

/// Helper: pre-flight check for faucet availability.
/// Returns the available balance or None if skipped.
async fn preflight_faucet(c: &Client, min_funds: i64) -> Option<i64> {
    let faucet_check = c
        .get(format!("{}/v1a/wallet/balance/", fullnode_url()))
        .send()
        .await;
    if faucet_check.is_err() {
        println!("SKIP: No local fullnode running at {}", fullnode_url());
        return None;
    }
    let faucet_balance: Value = faucet_check.unwrap().json().await.unwrap();
    let faucet_available = faucet_balance["balance"]
        .get("available")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if faucet_available < min_funds {
        println!(
            "SKIP: Faucet insufficient funds ({} < {})",
            format_htr(faucet_available),
            format_htr(min_funds)
        );
        return None;
    }
    Some(faucet_available)
}

/// Helper: fund a wallet and wait for confirmation.
async fn fund_and_wait(
    c: &Client,
    session: &Session,
    wallet_id: &str,
    address: &str,
    amount: i64,
) {
    let fund = send_from_faucet(c, address, amount).await;
    assert!(tx_success(&fund), "Faucet send failed: {:?}", fund);
    println!("  tx: {}", tx_hash(&fund));

    for i in 0..30 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let (avail, _) = get_balance(c, session, wallet_id).await;
        if avail >= amount {
            println!(
                "  confirmed after {}s - balance: {}",
                (i + 1) * 2,
                format_htr(avail)
            );
            return;
        }
    }
    panic!(
        "Wallet {} did not receive {} within 60s",
        wallet_id,
        format_htr(amount)
    );
}

/// Full e2e test: fund wallets from faucet, send between them.
/// Requires hathor-forge-cli running with --start (privatenet with funded faucet).
#[tokio::test]
async fn test_fund_and_transfer() {
    let c = client();
    let faucet_available = match preflight_faucet(&c, 1000).await {
        Some(v) => v,
        None => return,
    };

    println!("=== E2E TRANSFER TEST ===");
    println!("Faucet balance: {}", format_htr(faucet_available));

    // --- Setup: create isolated sessions + wallets ---
    println!("\n[setup] Creating isolated sessions...");
    let session_alice = create_session(&c).await;
    let session_bob = create_session(&c).await;
    println!("  Alice session: {} (api_key: {})", session_alice.id, session_alice.api_key);
    println!("  Bob session:   {} (api_key: {})", session_bob.id, session_bob.api_key);

    println!("[setup] Creating wallets...");
    create_wallet(&c, &session_alice, "alice", SEED_ALICE).await;
    create_wallet(&c, &session_bob, "bob", SEED_BOB).await;

    println!("[setup] Waiting for wallet sync...");
    wait_wallet_ready(&c, &session_alice, "alice").await;
    wait_wallet_ready(&c, &session_bob, "bob").await;

    let alice_addr = get_first_address(&c, &session_alice, "alice").await;
    let bob_addr = get_first_address(&c, &session_bob, "bob").await;
    println!("  Alice address: {}", alice_addr);
    println!("  Bob address:   {}", bob_addr);

    // === Step 1: Fund Alice from faucet (5 HTR) ===
    println!("\n[step 1] Faucet -> Alice: sending {}...", format_htr(500));
    let fund_result = send_from_faucet(&c, &alice_addr, 500).await;
    assert!(
        tx_success(&fund_result),
        "Faucet send failed: {:?}",
        fund_result
    );
    println!("  tx: {}", tx_hash(&fund_result));
    println!("  status: OK");

    println!("[step 1] Waiting for confirmation...");
    let mut alice_balance = (0i64, 0i64);
    for i in 0..30 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        alice_balance = get_balance(&c, &session_alice, "alice").await;
        if alice_balance.0 > 0 {
            println!(
                "  confirmed after {}s - Alice balance: {}",
                (i + 1) * 2,
                format_htr(alice_balance.0)
            );
            break;
        }
    }
    assert!(
        alice_balance.0 > 0,
        "Alice should have received funds from faucet"
    );

    // === Step 2: Alice sends 2 HTR to Bob ===
    println!("\n[step 2] Alice -> Bob: sending {}...", format_htr(200));
    let send_result = send_tx(&c, &session_alice, "alice", &bob_addr, 200).await;
    assert!(
        tx_success(&send_result),
        "Alice->Bob send failed: {:?}",
        send_result
    );
    println!("  tx: {}", tx_hash(&send_result));
    println!("  status: OK");

    println!("[step 2] Waiting for confirmation...");
    let mut bob_balance = (0i64, 0i64);
    for i in 0..30 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        bob_balance = get_balance(&c, &session_bob, "bob").await;
        if bob_balance.0 > 0 {
            println!(
                "  confirmed after {}s - Bob balance: {}",
                (i + 1) * 2,
                format_htr(bob_balance.0)
            );
            break;
        }
    }
    assert!(bob_balance.0 > 0, "Bob should have received funds from Alice");

    let alice_after_send = get_balance(&c, &session_alice, "alice").await;
    println!(
        "  Alice balance after send: {}",
        format_htr(alice_after_send.0)
    );
    assert!(
        alice_after_send.0 < alice_balance.0,
        "Alice should have less after sending to Bob"
    );

    // === Step 3: Bob sends 1 HTR back to Alice ===
    println!("\n[step 3] Bob -> Alice: sending {}...", format_htr(100));
    let send_back = send_tx(&c, &session_bob, "bob", &alice_addr, 100).await;
    assert!(
        tx_success(&send_back),
        "Bob->Alice send failed: {:?}",
        send_back
    );
    println!("  tx: {}", tx_hash(&send_back));
    println!("  status: OK");

    println!("[step 3] Waiting for confirmation...");
    let alice_before_return = alice_after_send.0;
    let mut alice_final = (0i64, 0i64);
    for i in 0..30 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        alice_final = get_balance(&c, &session_alice, "alice").await;
        if alice_final.0 > alice_before_return {
            println!(
                "  confirmed after {}s - Alice balance: {}",
                (i + 1) * 2,
                format_htr(alice_final.0)
            );
            break;
        }
    }
    assert!(
        alice_final.0 > alice_before_return,
        "Alice should have more after Bob sent funds back"
    );

    let bob_final = get_balance(&c, &session_bob, "bob").await;

    // === Summary ===
    println!("\n=== SUMMARY ===");
    println!("  Alice: {} (started at 0, funded 5.00, sent 2.00, received 1.00)", format_htr(alice_final.0));
    println!("  Bob:   {} (started at 0, received 2.00, sent 1.00)", format_htr(bob_final.0));
    println!("=== ALL TRANSFERS SUCCESSFUL ===");

    // Cleanup
    destroy_session(&c, &session_alice).await;
    destroy_session(&c, &session_bob).await;
}

// ============================================================================
// Token Tests
// ============================================================================

/// Helper: create a token and return (token_uid, token_hash).
async fn create_custom_token(
    c: &Client,
    session: &Session,
    wallet_id: &str,
    address: &str,
    name: &str,
    symbol: &str,
    amount: i64,
) -> (String, String) {
    let create_resp: Value = c
        .post(api_url(session, "/wallet/create-token"))
        .header("X-Wallet-Id", wallet_id)
        .json(&json!({
            "name": name,
            "symbol": symbol,
            "amount": amount,
            "address": address,
            "create_mint": true,
            "create_melt": true,
        }))
        .send()
        .await
        .expect("Failed to create token")
        .json()
        .await
        .expect("Failed to parse create-token response");

    assert!(
        create_resp
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "Token creation failed: {:?}",
        create_resp
    );

    // For create-token, the tx hash IS the token UID
    let token_uid = create_resp
        .get("hash")
        .and_then(|v| v.as_str())
        .expect("No hash in create-token response")
        .to_string();

    (token_uid.clone(), token_uid)
}

/// Helper: wait for a token balance to reach a target amount.
async fn wait_token_balance(
    c: &Client,
    session: &Session,
    wallet_id: &str,
    token: &str,
    target: i64,
    label: &str,
) -> i64 {
    for i in 0..30 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let (avail, _) = get_token_balance(c, session, wallet_id, Some(token)).await;
        if avail >= target {
            println!(
                "  {} confirmed after {}s - {} balance: {}",
                label,
                (i + 1) * 2,
                wallet_id,
                avail
            );
            return avail;
        }
    }
    let (avail, _) = get_token_balance(c, session, wallet_id, Some(token)).await;
    panic!(
        "{}: {} token balance is {} but expected >= {}",
        label, wallet_id, avail, target
    );
}

/// Create a custom token and verify the balance.
#[tokio::test]
async fn test_create_token() {
    let c = client();
    if preflight_faucet(&c, 200).await.is_none() {
        return;
    }

    println!("=== TOKEN CREATION TEST ===");

    let session = create_session(&c).await;
    println!("[setup] Session: {} (api_key: {})", session.id, session.api_key);

    create_wallet(&c, &session, "alice", SEED_ALICE).await;
    wait_wallet_ready(&c, &session, "alice").await;

    let alice_addr = get_first_address(&c, &session, "alice").await;
    println!("[setup] Alice address: {}", alice_addr);

    println!("\n[step 1] Funding Alice with {}...", format_htr(200));
    fund_and_wait(&c, &session, "alice", &alice_addr, 200).await;

    println!("\n[step 2] Creating custom token 'Test Token' (TST), 1000 units...");
    let (token_uid, token_hash) =
        create_custom_token(&c, &session, "alice", &alice_addr, "Test Token", "TST", 1000).await;
    println!("  tx: {}", token_hash);
    println!("  token_uid: {}", token_uid);

    // Verify token balance
    let balance = wait_token_balance(&c, &session, "alice", &token_uid, 1000, "creation").await;
    assert_eq!(balance, 1000, "Alice should have 1000 TST");

    println!("\n=== TOKEN CREATION SUCCESSFUL ===");
    destroy_session(&c, &session).await;
}

/// Transfer a custom token between two wallets in separate sessions.
#[tokio::test]
async fn test_token_transfer() {
    let c = client();
    if preflight_faucet(&c, 500).await.is_none() {
        return;
    }

    println!("=== TOKEN TRANSFER TEST ===");

    // Setup two sessions
    let session_alice = create_session(&c).await;
    let session_bob = create_session(&c).await;
    println!(
        "[setup] Alice session: {} | Bob session: {}",
        session_alice.id, session_bob.id
    );

    create_wallet(&c, &session_alice, "alice", SEED_ALICE).await;
    create_wallet(&c, &session_bob, "bob", SEED_BOB).await;
    wait_wallet_ready(&c, &session_alice, "alice").await;
    wait_wallet_ready(&c, &session_bob, "bob").await;

    let alice_addr = get_first_address(&c, &session_alice, "alice").await;
    let bob_addr = get_first_address(&c, &session_bob, "bob").await;
    println!("[setup] Alice: {} | Bob: {}", alice_addr, bob_addr);

    // Fund Alice (needs HTR for token creation deposit + transfer fees)
    println!("\n[step 1] Funding Alice with {}...", format_htr(500));
    fund_and_wait(&c, &session_alice, "alice", &alice_addr, 500).await;

    // Create token
    println!("\n[step 2] Creating token 'TransferCoin' (TFC), 500 units...");
    let (token_uid, token_hash) = create_custom_token(
        &c,
        &session_alice,
        "alice",
        &alice_addr,
        "TransferCoin",
        "TFC",
        500,
    )
    .await;
    println!("  tx: {}", token_hash);
    println!("  token_uid: {}", token_uid);
    wait_token_balance(&c, &session_alice, "alice", &token_uid, 500, "creation").await;

    // Send 200 TFC from Alice to Bob
    println!("\n[step 3] Alice -> Bob: sending 200 TFC...");
    let send_resp: Value = c
        .post(api_url(&session_alice, "/wallet/send-tx"))
        .header("X-Wallet-Id", "alice")
        .json(&json!({
            "outputs": [{
                "address": bob_addr,
                "value": 200,
                "token": token_uid,
            }],
        }))
        .send()
        .await
        .expect("Failed to send token")
        .json()
        .await
        .expect("Failed to parse send response");

    assert!(
        tx_success(&send_resp),
        "Token send failed: {:?}",
        send_resp
    );
    println!("  tx: {}", tx_hash(&send_resp));

    // Wait for Bob to receive tokens
    println!("[step 3] Waiting for Bob to receive TFC...");
    let bob_tfc = wait_token_balance(&c, &session_bob, "bob", &token_uid, 200, "transfer").await;
    assert_eq!(bob_tfc, 200, "Bob should have 200 TFC");

    // Check Alice's remaining balance
    let (alice_tfc, _) =
        get_token_balance(&c, &session_alice, "alice", Some(&token_uid)).await;
    println!("  Alice TFC balance: {}", alice_tfc);
    assert_eq!(alice_tfc, 300, "Alice should have 300 TFC remaining");

    // Send 50 TFC back from Bob to Alice
    println!("\n[step 4] Bob -> Alice: sending 50 TFC...");

    // Bob needs a tiny bit of HTR for the tx fee
    println!("[step 4] Funding Bob with {} for fee...", format_htr(100));
    fund_and_wait(&c, &session_bob, "bob", &bob_addr, 100).await;

    let send_back: Value = c
        .post(api_url(&session_bob, "/wallet/send-tx"))
        .header("X-Wallet-Id", "bob")
        .json(&json!({
            "outputs": [{
                "address": alice_addr,
                "value": 50,
                "token": token_uid,
            }],
        }))
        .send()
        .await
        .expect("Failed to send token back")
        .json()
        .await
        .expect("Failed to parse send response");

    assert!(
        tx_success(&send_back),
        "Token send back failed: {:?}",
        send_back
    );
    println!("  tx: {}", tx_hash(&send_back));

    // Verify final balances
    println!("[step 4] Waiting for confirmation...");
    let alice_final =
        wait_token_balance(&c, &session_alice, "alice", &token_uid, 350, "return").await;
    let (bob_final, _) = get_token_balance(&c, &session_bob, "bob", Some(&token_uid)).await;

    println!("\n=== SUMMARY ===");
    println!(
        "  Alice TFC: {} (created 500, sent 200, received 50)",
        alice_final
    );
    println!("  Bob TFC:   {} (received 200, sent 50)", bob_final);
    assert_eq!(alice_final, 350);
    assert_eq!(bob_final, 150);
    println!("=== TOKEN TRANSFER SUCCESSFUL ===");

    destroy_session(&c, &session_alice).await;
    destroy_session(&c, &session_bob).await;
}
