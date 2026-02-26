FROM rust:slim AS planner
WORKDIR /app

RUN cargo install cargo-chef

FROM planner AS cacher
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM rust:slim AS builder

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
    echo "index = 'sparse+https://mirrors.ustc.edu.cn/crates.io-index/'" >> ~/.cargo/config.toml \
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
COPY --from=cacher /app/recipe.json recipe.json

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
COPY --from=builder /app/settings.json ./

ENV HOST=srt.example.local

ENV GRPC_PORT=50051
ENV RTMP_PORT=1935
ENV SRT_PORTS=4000-4100

ENV MINIO_URI=http://localhost:9000
ENV MINIO_ACCESSKEY=minioadmin
ENV MINIO_SECRETKEY=miniokey
ENV MINIO_BUCKET=videos

ENV RUST_LOG=info
ENV SEGMENT_TIME=10

ENTRYPOINT ["./livestream-rs"]
