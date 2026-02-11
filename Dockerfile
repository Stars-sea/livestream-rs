FROM rust:slim AS builder

RUN sed -i 's/deb.debian.org/mirrors.ustc.edu.cn/g' /etc/apt/sources.list.d/debian.sources && \
    sed -i 's/bookworm/trixie/g' /etc/apt/sources.list.d/debian.sources

RUN apt-get update && apt-get dist-upgrade -y && apt-get install -y \
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

# Configure Cargo mirrors
RUN mkdir -p ~/.cargo && \
    echo "[source.crates-io]" > ~/.cargo/config.toml && \
    echo "replace-with = 'tuna'" >> ~/.cargo/config.toml && \
    echo "[source.tuna]" >> ~/.cargo/config.toml && \
    echo "registry = 'sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/'" >> ~/.cargo/config.toml && \
    echo "[registries.mirror]" >> ~/.cargo/config.toml && \
    echo "index = 'sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/'" >> ~/.cargo/config.toml

WORKDIR /app
COPY . ./

# Build with release profile (dynamic linking)
RUN cargo build --release && \
    strip target/release/livestream-rs

FROM debian:trixie-slim

RUN sed -i 's/deb.debian.org/mirrors.ustc.edu.cn/g' /etc/apt/sources.list.d/debian.sources

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    tzdata \
    ffmpeg \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/livestream-rs ./
COPY --from=builder /app/settings.json ./

ENV GRPC_PORT=50051
ENV MINIO_ENDPOINT=http://localhost:9000
ENV MINIO_ACCESS_KEY=minioadmin
ENV MINIO_SECRET_KEY=miniokey
ENV MINIO_BUCKET=videos
ENV RUST_LOG=info

RUN mkdir ./cache

ENTRYPOINT ["./livestream-rs"]
