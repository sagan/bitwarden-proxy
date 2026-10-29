FROM rust:1-slim-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN useradd --system --uid 10001 --create-home bitwarden-proxy

COPY --from=builder /app/target/release/bitwarden-proxy /usr/local/bin/bitwarden-proxy

USER bitwarden-proxy
EXPOSE 8080
ENV BITWARDEN_PROXY_BIND=0.0.0.0:8080

ENTRYPOINT ["/usr/local/bin/bitwarden-proxy"]
