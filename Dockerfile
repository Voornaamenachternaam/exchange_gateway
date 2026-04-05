# Dockerfile
FROM rust:1.94.1-slim AS builder
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml ./
RUN mkdir -p src && echo 'fn main(){}' > src/main.rs
RUN cargo build --release
RUN rm -rf src target/release/deps/exchange_gateway* target/release/exchange_gateway*
COPY src ./src
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates tzdata curl libssl3 && rm -rf /var/lib/apt/lists/*
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
  CMD curl -f http://localhost:8134/health || exit 1
CMD ["/usr/local/bin/exchange_gateway"]
