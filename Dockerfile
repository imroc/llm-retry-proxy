FROM rust:1.89-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/llm-retry-proxy /usr/local/bin/
COPY --from=builder /app/config.example.toml /etc/llm-retry-proxy/config.toml

EXPOSE 8888
ENTRYPOINT ["llm-retry-proxy", "--config", "/etc/llm-retry-proxy/config.toml"]
