# Dockerfile
# Gap 8 (Security hardening): non-root runtime user, health check, clean dependency cache.

FROM rust:1.94.0-slim AS builder
WORKDIR /app
COPY Cargo.toml ./
RUN mkdir -p src && echo 'fn main(){}' > src/main.rs
RUN cargo fetch
RUN rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates tzdata curl && rm -rf /var/lib/apt/lists/*
RUN groupadd --system --gid 10001 gateway \
    && useradd --system --uid 10001 --gid gateway \
       --shell /usr/sbin/nologin --no-create-home gateway
RUN mkdir -p /etc/exchange-gateway \
    && chown root:gateway /etc/exchange-gateway \
    && chmod 750 /etc/exchange-gateway
COPY --from=builder /app/target/release/exchange_gateway /usr/local/bin/exchange_gateway
RUN chmod 755 /usr/local/bin/exchange_gateway
ENV RUST_LOG="info"
ENV TZ="UTC"
USER gateway
EXPOSE 8134
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8134/ || exit 1
CMD ["/usr/local/bin/exchange_gateway"]
