# Multi-stage build for savr-server. Only savr-server (+ savr-core) is
# compiled — `-p savr-server` never pulls the daemon/app dependency trees.
FROM rust:1-slim-bookworm AS build
WORKDIR /app
COPY . .
RUN cargo build --release -p savr-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/savr-server /usr/local/bin/savr-server

# SQLite db + content-addressed blob store live here.
VOLUME ["/data"]
ENV SAVR_DB_URL="sqlite:///data/savr.db" \
    SAVR_BLOB_PATH="/data/blobs" \
    SAVR_BIND="0.0.0.0:8080"
EXPOSE 8080

# ponytail: no HEALTHCHECK curl in the image (keeps it tiny, no curl); compose
# / orchestrator can probe /healthz over HTTP instead.
ENTRYPOINT ["savr-server"]
