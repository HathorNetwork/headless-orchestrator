# Headless Orchestrator

Multi-tenant wallet-headless orchestrator for Hathor Network. Spins up isolated Docker containers per user session so that each user gets their own wallet-headless instance — no cross-tenant wallet access.

## The Problem

`hathor-wallet-headless` is not multi-tenant. Any client connected to it can access any wallet created on it. Hosting a single shared instance would let users access wallets they don't own.

## The Solution

The orchestrator manages the lifecycle of per-session wallet-headless Docker containers:

1. **User requests a session** → orchestrator spawns a fresh `hathor-wallet-headless` container
2. **All wallet API calls** are proxied through the orchestrator to the user's container
3. **Idle containers** are automatically reaped after a configurable timeout
4. **Session cleanup** destroys the container and all wallet state

```
┌──────────────┐     ┌──────────────────────┐     ┌─────────────────┐
│  hathor-mcp  │────▶│ headless-orchestrator │────▶│ Docker container│
│  (per user)  │     │                      │     │ wallet-headless │
└──────────────┘     │  POST /sessions      │     │ (isolated)      │
                     │  → spawn container   │     └────────┬────────┘
                     │                      │              │
                     │  /sessions/:id/api/* │              │
                     │  → proxy to container│──────────────┘
                     │                      │
                     │  DELETE /sessions/:id │     ┌─────────────────┐
                     │  → kill container    │     │ Hathor Fullnode  │
                     └──────────────────────┘     │ (shared)        │
                                                  └─────────────────┘
```

## Quick Start

```bash
# Build
cargo build --release

# Run (connects containers to fullnode at localhost:8080)
./target/release/headless-orchestrator \
  --fullnode-url http://host.docker.internal:8080/v1a/ \
  --network privatenet

# With tx-mining support
./target/release/headless-orchestrator \
  --fullnode-url http://host.docker.internal:8080/v1a/ \
  --tx-mining-url http://host.docker.internal:8002 \
  --network privatenet
```

## API

### Session Management

```bash
# Create a new session (spawns a container)
curl -X POST http://localhost:8100/sessions
# → {"session_id": "abc-123", "message": "Wallet-headless instance is ready"}

# List active sessions
curl http://localhost:8100/sessions
# → {"sessions": [{"session_id": "abc-123", "port": 32768, "idle_secs": 42}]}

# Destroy a session (kills the container)
curl -X DELETE http://localhost:8100/sessions/abc-123
# → {"destroyed": true, "session_id": "abc-123"}
```

### Wallet API (proxied)

All wallet-headless endpoints are available under `/sessions/:session_id/api/`:

```bash
SESSION="abc-123"
BASE="http://localhost:8100/sessions/$SESSION/api"

# Create a wallet
curl -X POST "$BASE/start" \
  -H "Content-Type: application/json" \
  -d '{"wallet-id": "my-wallet", "seed": "your 24 word seed..."}'

# Check wallet status
curl "$BASE/wallet/status" -H "X-Wallet-Id: my-wallet"

# Get balance
curl "$BASE/wallet/balance" -H "X-Wallet-Id: my-wallet"

# Send transaction
curl -X POST "$BASE/wallet/simple-send-tx" \
  -H "X-Wallet-Id: my-wallet" \
  -H "Content-Type: application/json" \
  -d '{"address": "WXk...", "value": 100}'
```

## CLI Options

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | 8100 | Orchestrator HTTP port |
| `--fullnode-url` | http://host.docker.internal:8080/v1a/ | Fullnode URL for containers |
| `--tx-mining-url` | (none) | Tx-mining service URL |
| `--network` | privatenet | Network name |
| `--headless-image` | hathornetwork/hathor-wallet-headless:latest | Docker image |
| `--docker-network` | (none) | Custom Docker network |
| `--max-instances` | 100 | Max concurrent containers (0=unlimited) |
| `--idle-timeout-secs` | 1800 | Kill idle containers after N seconds |

## How It Integrates With hathor-mcp

The MCP server should be configured to use the orchestrator as its wallet-headless backend:

1. On session start, MCP calls `POST /sessions` to get a `session_id`
2. All wallet-headless API calls go through `http://orchestrator:8100/sessions/{session_id}/api/`
3. On session end, MCP calls `DELETE /sessions/{session_id}`

## License

MIT
