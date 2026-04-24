FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

ARG USE_MIRROR=true

# Conditionally configure mirrors
RUN if [ "$USE_MIRROR" = "true" ]; then \
    sed -i 's/deb.debian.org/mirrors.ustc.edu.cn/g' /etc/apt/sources.list.d/debian.sources && \
    sed -i 's/bookworm/trixie/g' /etc/apt/sources.list.d/debian.sources && \
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
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json

# Build dependencies - this is the caching layer!
RUN cargo chef cook --release --recipe-path recipe.json

# Build application
COPY . .
RUN cargo build --release && \
    strip target/release/livestream-rs

FROM debian:trixie-slim

ARG USE_MIRROR=true

RUN if [ "$USE_MIRROR" = "true" ]; then \
    sed -i 's/deb.debian.org/mirrors.ustc.edu.cn/g' /etc/apt/sources.list.d/debian.sources; \
    fi

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    tzdata \
    ffmpeg \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/livestream-rs ./

ENV SRT__PORTS=4000-4100

ENV PERSISTENCE__DURATION=10
ENV PERSISTENCE__CACHE_DIR=

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

ENV MINIO_URI=http://localhost:9000
ENV MINIO_ACCESSKEY=minioadmin
ENV MINIO_SECRETKEY=miniokey
ENV MINIO_BUCKET=videos

ENV RUST_LOG=info

ENTRYPOINT ["./livestream-rs"]
