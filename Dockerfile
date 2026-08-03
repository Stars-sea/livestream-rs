# cargo-chef has no prebuilt image for rust 1.97 (v0.1.77 predates it), so
# install the pinned cargo-chef version into the official rust image instead.
FROM rust:1.97-slim AS chef
RUN cargo install cargo-chef --locked --version 0.1.77
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

ARG USE_MIRROR=true

# Conditionally configure CN mirrors (build-time only; CI builds with
# USE_MIRROR=false against official sources).
RUN if [ "$USE_MIRROR" = "true" ]; then \
    sed -i 's/deb.debian.org/mirrors.ustc.edu.cn/g' /etc/apt/sources.list.d/debian.sources && \
    mkdir -p ~/.cargo && \
    echo "[source.crates-io]" > ~/.cargo/config.toml && \
    echo "replace-with = 'tuna'" >> ~/.cargo/config.toml && \
    echo "[source.tuna]" >> ~/.cargo/config.toml && \
    echo "registry = 'sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/'" >> ~/.cargo/config.toml && \
    echo "[registries.tuna]" >> ~/.cargo/config.toml && \
    echo "index = 'sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/'" >> ~/.cargo/config.toml && \
    echo "[source.ustc]" >> ~/.cargo/config.toml && \
    echo "registry = 'sparse+https://mirrors.ustc.edu.cn/crates.io-index/'" >> ~/.cargo/config.toml && \
    echo "[registries.ustc]" >> ~/.cargo/config.toml && \
    echo "index = 'sparse+https://mirrors.ustc.edu.cn/crates.io-index/'" >> ~/.cargo/config.toml; \
    fi

RUN apt-get update && apt-get install -y \
    build-essential \
    clang \
    libclang-dev \
    pkg-config \
    libssl-dev \
    libavcodec-dev \
    libavformat-dev \
    libavutil-dev \
    libswscale-dev \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json

# Build dependencies - this is the caching layer!
RUN cargo chef cook --release --recipe-path recipe.json

# Build application
COPY . .
RUN cargo build --release && \
    strip target/release/livestream

FROM debian:trixie-slim

ARG USE_MIRROR=true

RUN if [ "$USE_MIRROR" = "true" ]; then \
    sed -i 's/deb.debian.org/mirrors.ustc.edu.cn/g' /etc/apt/sources.list.d/debian.sources; \
    fi

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    tzdata \
    curl \
    ffmpeg \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 livestream

WORKDIR /app
COPY --from=builder /app/target/release/livestream ./

# HLS segment production (replaces the removed SRT/persistence config sections).
# AppConfig flattens `storage`, so the keys are SEGMENT__*, not STORAGE__SEGMENT__*.
ENV SEGMENT__DURATION_SECS=10
ENV SEGMENT__CACHE_DIR=/tmp/hls-segments

ENV GRPC__PORT=50051

ENV RTMP__PORT=1935
ENV RTMP__APP_NAME=lives
ENV RTMP__SESSION_TTL_SECS=30

ENV HTTP_FLV__ENABLED=true
ENV HTTP_FLV__PORT=8080

ENV QUEUE__RTMP_FORWARD=8192
ENV QUEUE__FLV_RELAY=2048
ENV QUEUE__PACKET_RELAY=2048
ENV QUEUE__CONTROL=1024
ENV QUEUE__EVENT=4096

ENV RUST_LOG=info

# No MINIO_* defaults: storage credentials must be provided explicitly by
# the operator (missing config degrades to FLV-only with a warning log).
# GRPC__AUTH_TOKEN is also intentionally unset — set a strong token for any
# deployment that exposes port 50051.

USER livestream

EXPOSE 1935 8554 8080 50051

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8080/alive > /dev/null || exit 1

ENTRYPOINT ["./livestream"]
