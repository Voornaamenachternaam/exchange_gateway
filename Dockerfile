# Dockerfile - Optimized for May 2026
# Multi-stage build with proper layer caching and BuildKit optimizations

# Build stage with rustup for toolchain management
FROM rust:1.95.0-slim AS builder

WORKDIR /app

# Enable sparse registry protocol for faster downloads
ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
ENV RUSTFLAGS="-D warnings"
ENV CARGO_TERM_COLOR=always

# Copy dependency files first for optimal layer caching
COPY Cargo.toml Cargo.lock ./

# Copy minimal lib.rs for dependency caching
COPY src/lib.rs ./src/lib.rs

# Install dependencies - this layer will be reused as long as Cargo.toml/Cargo.lock don't change
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo fetch --locked

# Copy remaining source files
COPY src/ ./src/
COPY sqlite_schema.sql ./

# Build the application with cache mounts
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release --bin exchange_gateway

# Runtime stage - ultra-minimal and secure
FROM debian:trixie-slim AS runtime

# Install runtime dependencies with security updates
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    tzdata \
    libsqlite3-0 \
    curl \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user and directories with proper permissions
RUN groupadd --system --gid 10001 gateway && \
    useradd --system --uid 10001 --gid gateway \
    --shell /usr/sbin/nologin --no-create-home gateway && \
    mkdir -p /etc/exchange-gateway /var/lib/exchange-gateway && \
    chown root:gateway /etc/exchange-gateway /var/lib/exchange-gateway && \
    chmod 750 /etc/exchange-gateway && \
    chmod 770 /var/lib/exchange-gateway

# Copy compiled binary from builder
COPY --from=builder --chown=root:root /app/target/release/exchange_gateway /usr/local/bin/exchange_gateway

# Environment configuration
ENV RUST_LOG="info"
ENV TZ="UTC"

# Security hardening
RUN chmod 755 /usr/local/bin/exchange_gateway

# Switch to non-root user for security
USER gateway
WORKDIR /var/lib/exchange-gateway

# Network configuration
EXPOSE 8134

# Health check with proper settings
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8134/health || exit 1

# Entrypoint
ENTRYPOINT ["/usr/local/bin/exchange_gateway"]