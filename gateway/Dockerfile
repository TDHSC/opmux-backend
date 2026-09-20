# Locked, MSRV-compatible gateway image.
# Builder is the verified Rust 1.89.0 Bookworm image. Runtime is Debian
# Bookworm slim with CA roots and OpenSSL. The process runs as UID 65532.
# Do not bake DATABASE_URL, provider keys, or AUTH_DEVELOPMENT_MODE.
# Local plain-HTTP simulators and sslmode=disable database URLs are
# local-only; they are not hosted or provider TLS proof.
FROM rust:1.89.0-bookworm@sha256:948f9b08a66e7fe01b03a98ef1c7568292e07ec2e4fe90d88c07bb14563c84ff AS builder

WORKDIR /src

ARG CARGO_BUILD_JOBS=2
ENV CARGO_TERM_COLOR=never \
    CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}

COPY Cargo.toml Cargo.lock ./
COPY common ./common
COPY gateway/Cargo.toml gateway/Cargo.toml
COPY gateway/src ./gateway/src

RUN cargo build --locked --release -p gateway \
    && install -D -m 0755 target/release/gateway /out/gateway \
    && install -D -m 0755 target/release/opmux-admin /out/opmux-admin

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 65532 opmux \
    && useradd --system --uid 65532 --gid opmux --home-dir /nonexistent \
        --no-create-home --shell /usr/sbin/nologin opmux \
    && mkdir -p /app/config \
    && chown opmux:opmux /app /app/config

COPY --from=builder /out/gateway /usr/local/bin/gateway
COPY --from=builder /out/opmux-admin /usr/local/bin/opmux-admin
COPY --chown=opmux:opmux --chmod=0644 config/opmux.example.json /app/config/opmux.example.json

USER 65532:65532
WORKDIR /app

ENV SERVER_HOST=0.0.0.0 \
    SERVER_PORT=3000 \
    OPMUX_CONFIG_FILE=/app/config/opmux.example.json

EXPOSE 3000

CMD ["gateway"]
