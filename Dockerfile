FROM rust:1.94.1-slim AS builder
WORKDIR /app
COPY Cargo.toml ./
RUN mkdir -p src
COPY src/ ./src/
RUN cargo build --release

FROM debian:bookworm-slim
WORKDIR /app

# Security: Install only required packages, no unnecessary tools
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates tzdata curl && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

# Create non-root user for security
RUN useradd -m -s /bin/false appuser && \
    mkdir -p /etc/exchange-gateway && \
    chown -R appuser:appuser /app /etc/exchange-gateway

COPY --from=builder /app/target/release/exchange_gateway /usr/local/bin/exchange_gateway

# Environment variables
ENV RUST_LOG="info" \
    RUST_BACKTRACE="1"

# Security: Run as non-root user
USER appuser

# Expose the gateway port
EXPOSE 8134

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8134/health || exit 1

CMD ["/usr/local/bin/exchange_gateway"]
