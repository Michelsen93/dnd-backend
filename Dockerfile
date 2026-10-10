# Pixel Quest API for Cloud Run: the Rust server + Litestream (SQLite → Cloud Storage replication).

FROM rust:1.95-bookworm AS build
ARG LITESTREAM_VERSION=0.3.13
RUN curl -fsSL "https://github.com/benbjohnson/litestream/releases/download/v${LITESTREAM_VERSION}/litestream-v${LITESTREAM_VERSION}-linux-amd64.tar.gz" \
    | tar -xz -C /usr/local/bin litestream
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --release --locked

FROM debian:bookworm-slim
# CA roots for Litestream's TLS connection to Cloud Storage.
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /usr/local/bin/litestream /usr/local/bin/litestream
COPY --from=build /src/target/release/backend /app/backend
COPY docker-entrypoint.sh /app/docker-entrypoint.sh
RUN chmod +x /app/docker-entrypoint.sh && mkdir -p /data

ENV APP_HOST=0.0.0.0 \
    DB_PATH=/data/dnd.sqlite \
    RUST_LOG=backend=info,tower_http=warn
EXPOSE 8080
ENTRYPOINT ["/app/docker-entrypoint.sh"]
