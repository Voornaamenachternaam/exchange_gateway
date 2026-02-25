FROM rust:1.93.1-bullseye AS builder
WORKDIR /usr/src/exchange_gateway
RUN apt-get update && apt-get install -y pkg-config libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*

# Copy manifests
COPY Cargo.toml ./
# Create dummy main.rs to build dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release

# Copy actual source
COPY src ./src
RUN rm -f target/release/exchange_gateway
RUN cargo build --release

FROM debian:stable-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app

# Copy binary and ensure it is executable
COPY --from=builder /usr/src/exchange_gateway/target/release/exchange_gateway /usr/local/bin/exchange_gateway
RUN chmod +x /usr/local/bin/exchange_gateway

# Create config directory
RUN mkdir -p /etc/exchange-gateway

# Copy default config (will be overridden by volume mount)
COPY config.toml /etc/exchange-gateway/config.toml

EXPOSE 8133
USER 1000:1000

# Entrypoint points to the location used in main.rs
ENTRYPOINT ["/usr/local/bin/exchange_gateway"]
