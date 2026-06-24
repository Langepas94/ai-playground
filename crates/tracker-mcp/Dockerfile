# Multi-stage build: Rust cargo on full image, copy only binary to slim runtime.
# Result: ~100 МБ образ для Yandex Serverless Containers.

# Stage 1: Build
FROM rust:1.82-alpine AS builder

WORKDIR /build

# Install build deps (musl for static linking, pkg-config for native-tls)
RUN apk add --no-cache musl-dev pkg-config openssl-dev

# Copy workspace + crate
COPY Cargo.lock Cargo.toml ./
COPY crates/tracker-mcp ./crates/tracker-mcp/
COPY src ./src/

# Build binary (release mode)
RUN cargo build --release -p tracker-mcp --bin tracker-mcp-http

# Stage 2: Runtime
FROM alpine:3.20

WORKDIR /app

# Install runtime deps (ca-certificates for HTTPS, openssl for native-tls)
RUN apk add --no-cache ca-certificates libssl3

# Copy binary from builder
COPY --from=builder /build/target/release/tracker-mcp-http .

# MCP listens on PORT (default 8080)
EXPOSE 8080

# Required env: TRACKER_TOKEN, TRACKER_ORG_ID
# Optional: TRACKER_TOKEN_KIND (default oauth), TRACKER_ORG_KIND (default x_org_id),
#           TRACKER_BASE_URL (default prod), MCP_AUTH_TOKEN (required for security),
#           MCP_ALLOW_NO_AUTH, MCP_ALLOWED_HOSTS, MCP_STATELESS (for serverless)
ENV PORT=8080

CMD ["./tracker-mcp-http"]
