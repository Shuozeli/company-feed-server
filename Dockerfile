FROM golang:1.26-bookworm AS schema-builder

ARG PG_SCHEMA_DIFF_VERSION=v1.0.8
RUN GOBIN=/out go install \
      github.com/stripe/pg-schema-diff/cmd/pg-schema-diff@${PG_SCHEMA_DIFF_VERSION}

FROM rust:1.95-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY bins ./bins
COPY schema ./schema
COPY docs/news-viewer.html ./docs/news-viewer.html
RUN --mount=type=cache,id=company-feed-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=company-feed-target,target=/app/target \
    cargo build --locked --release \
      -p feed-server \
      -p feed-worker \
      -p feed-discovery-worker \
      -p feed-validation-worker \
      -p feed-news-extraction-worker \
      -p feed-content-worker \
      -p feed-admin \
      -p feed-payload-offloader \
    && mkdir -p /app/out \
    && cp /app/target/release/feed-server /app/out/feed-server \
    && cp /app/target/release/feed-worker /app/out/feed-worker \
    && cp /app/target/release/feed-discovery-worker /app/out/feed-discovery-worker \
    && cp /app/target/release/feed-validation-worker /app/out/feed-validation-worker \
    && cp /app/target/release/feed-news-extraction-worker /app/out/feed-news-extraction-worker \
    && cp /app/target/release/feed-content-worker /app/out/feed-content-worker \
    && cp /app/target/release/feed-admin /app/out/feed-admin \
    && cp /app/target/release/feed-payload-offloader /app/out/feed-payload-offloader

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl git openssh-client \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 company-feed
WORKDIR /app
COPY --from=builder /app/out/feed-server /usr/local/bin/feed-server
COPY --from=builder /app/out/feed-worker /usr/local/bin/feed-worker
COPY --from=builder /app/out/feed-discovery-worker /usr/local/bin/feed-discovery-worker
COPY --from=builder /app/out/feed-validation-worker /usr/local/bin/feed-validation-worker
COPY --from=builder /app/out/feed-news-extraction-worker /usr/local/bin/feed-news-extraction-worker
COPY --from=builder /app/out/feed-content-worker /usr/local/bin/feed-content-worker
COPY --from=builder /app/out/feed-admin /usr/local/bin/feed-admin
COPY --from=builder /app/out/feed-payload-offloader /usr/local/bin/feed-payload-offloader
COPY --from=schema-builder /out/pg-schema-diff /usr/local/bin/pg-schema-diff
COPY configs ./configs
COPY schema ./schema

RUN mkdir -p /app/exports \
    && chown -R company-feed:company-feed /app

USER company-feed
EXPOSE 8080 8081 8082 8083 8084 8085
ENTRYPOINT ["feed-server"]
