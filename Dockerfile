FROM rust:1.91.1-bookworm AS builder
WORKDIR /usr/src/exchange_gateway
RUN apt-get update && apt-get install -y pkg-config libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY config.toml ./config.toml

RUN cargo build --release

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /usr/src/exchange_gateway/target/release/exchange_gateway /usr/local/bin/exchange_gateway
COPY config.toml /etc/exchange-gateway/config.toml
EXPOSE 8443 8080
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/exchange_gateway", "--config", "/etc/exchange-gateway/config.toml"]
