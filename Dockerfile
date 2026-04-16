# Multi-stage build for headless-orchestrator
FROM rust:1.83-bookworm AS builder

WORKDIR /app

# Pre-cache deps by copying manifests first
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src target/release/deps/headless_orchestrator* target/release/headless-orchestrator*

# Real build
COPY src ./src
COPY tests ./tests
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/headless-orchestrator /usr/local/bin/headless-orchestrator

EXPOSE 8100

ENTRYPOINT ["headless-orchestrator"]
