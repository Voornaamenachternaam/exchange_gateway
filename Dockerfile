# Dockerfile - Optimized for May 2026
# Multi-stage build with proper layer caching and BuildKit optimizations

# Build stage with rustup for toolchain management
FROM rust:1.95.0-slim AS builder

WORKDIR /app

# Enable sparse registry protocol for faster downloads
ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
ENV RUSTFLAGS="-D warnings"

# Copy dependency files first for optimal layer caching
COPY Cargo.toml Cargo.lock ./

# Copy minimal lib.rs for dependency caching
COPY src/lib.rs ./src/lib.rs

# Install dependencies - this layer will be reused as long as Cargo.toml/Cargo.lock don't change
RUN cargo fetch --locked

# Copy remaining source files
COPY src/ ./src/
COPY sqlite_schema.sql ./

# Build the application
RUN cargo build --release --bin exchange_gateway

# Runtime stage - ultra-minimal and secure
FROM debian:trixie-slim AS runtime

# Install runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    tzdata \
    libsqlite3-0 \
    curl \
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
# Do NOT set RUST_LOG here — it takes priority over GATEWAY_LOG_LEVEL in
# tracing_subscriber's EnvFilter::try_from_default_env(), silently
# overriding the user's explicit log level. The gateway's logging.rs
# already defaults to "info" when neither RUST_LOG nor GATEWAY_LOG_LEVEL
# is set, so a baked-in RUST_LOG=info is redundant AND harmful.
ENV TZ="UTC"

# Switch to non-root user for security
USER gateway

# Network configuration
EXPOSE 8134

# Health check with proper settings
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8134/health || exit 1

# Entrypoint
ENTRYPOINT ["/usr/local/bin/exchange_gateway"]