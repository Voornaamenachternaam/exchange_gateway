FROM rust:1.94.0-slim AS builder
WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y ca-certificates tzdata && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /etc/exchange-gateway
COPY --from=builder /app/target/release/exchange_gateway /usr/local/bin/exchange_gateway
ENV RUST_LOG="info"
EXPOSE 8134
CMD ["/usr/local/bin/exchange_gateway"]
