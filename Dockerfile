# syntax=docker/dockerfile:1.7

ARG RUST_VERSION=1
ARG PYTHON_VERSION=3.12
ARG SYNCPLAY_VERSION=1.7.5
ARG TAILSCALE_TAG=stable


FROM rust:${RUST_VERSION}-slim-bookworm AS builder

WORKDIR /build

COPY src-tauri/Cargo.toml src-tauri/Cargo.lock ./
COPY src-tauri/build.rs ./build.rs
COPY src-tauri/src ./src

# The headless feature set drops Tauri, the webview and the keychain.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --release --locked --no-default-features --features headless \
        --bin syncpartyd \
    && install -Dm0755 target/release/syncpartyd /out/syncpartyd


# Multi-arch resolves at the registry, so no TARGETARCH mapping needed.
FROM tailscale/tailscale:${TAILSCALE_TAG} AS tailscale


FROM python:${PYTHON_VERSION}-slim-bookworm

ARG SYNCPLAY_VERSION

# iptables and iproute2 are tailscaled's; it programs both to route the tunnel.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates iptables iproute2 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=tailscale /usr/local/bin/tailscaled /usr/local/bin/tailscaled
COPY --from=tailscale /usr/local/bin/tailscale /usr/local/bin/tailscale

# requirements.txt is the server's alone; the Qt client deps live elsewhere.
ADD https://github.com/Syncplay/syncplay/archive/refs/tags/v${SYNCPLAY_VERSION}.tar.gz \
    /tmp/syncplay.tar.gz

RUN mkdir -p /opt/syncplay \
    && tar -xzf /tmp/syncplay.tar.gz -C /opt/syncplay --strip-components=1 \
    && rm /tmp/syncplay.tar.gz \
    && python -m venv /opt/syncplay-venv \
    && /opt/syncplay-venv/bin/pip install --no-cache-dir --upgrade pip \
    && /opt/syncplay-venv/bin/pip install --no-cache-dir -r /opt/syncplay/requirements.txt

COPY --from=builder /out/syncpartyd /usr/local/bin/syncpartyd
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 0755 /usr/local/bin/entrypoint.sh

# /data must be a mounted volume — it holds the server password and salt.
ENV SYNCPARTY_DATA_DIR=/data \
    SYNCPARTY_SERVER_PYTHON=/opt/syncplay-venv/bin/python \
    SYNCPARTY_SERVER_ENTRYPOINT=/opt/syncplay/syncplayServer.py \
    TS_STATE_DIR=/data/tailscale \
    TS_HOSTNAME=syncparty \
    SYNCPARTY_PORT=8999 \
    PYTHONIOENCODING=utf-8

RUN mkdir -p /data

HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD /usr/local/bin/entrypoint.sh healthcheck

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["syncpartyd"]
