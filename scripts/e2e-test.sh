#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
TEST_INPUT="${TEST_INPUT:-$PROJECT_DIR/testdata/sample.mp4}"
GRPC_PORT="${GRPC_PORT:-50051}"
RTMP_PORT="${RTMP_PORT:-11935}"
RTSP_PORT="${RTSP_PORT:-8554}"
HTTP_FLV_PORT="${HTTP_FLV_PORT:-8080}"

# The server reads its ports through the config crate's "__" separator
# convention (RTMP__PORT -> rtmp.port), so export the double-underscore names
# for the background process below. The plain *_PORT variables above stay
# script-local for probing and for the test client's --grpc-addr.
export GRPC__PORT="$GRPC_PORT"
export RTMP__PORT="$RTMP_PORT"
export RTSP__PORT="$RTSP_PORT"
export HTTP_FLV__PORT="$HTTP_FLV_PORT"
# HTTP-FLV playback is opt-in (disabled by default when no config.toml).
export HTTP_FLV__ENABLED=true

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

log()  { echo -e "${GREEN}[e2e]${NC} $*"; }
err()  { echo -e "${RED}[e2e]${NC} $*" >&2; }

# ── Preflight ──
if ! command -v ffmpeg &>/dev/null; then
    err "ffmpeg 未安装, 退出"
    exit 1
fi

if ! command -v cargo &>/dev/null; then
    err "cargo 未安装, 退出"
    exit 1
fi

# ── 1. Build ──
log "构建 livestream 和 stress-test..."
cargo build --release -p livestream -p livestream-test-utils

# ── 2. Generate test input if missing ──
if [ ! -f "$TEST_INPUT" ]; then
    log "生成测试视频: $TEST_INPUT"
    mkdir -p "$(dirname "$TEST_INPUT")"
    ffmpeg -y -f lavfi -i "testsrc=duration=30:size=1280x720:rate=30" \
           -f lavfi -i "sine=frequency=440:duration=30" \
           -c:v libx264 -preset veryfast -g 30 -pix_fmt yuv420p \
           -c:a aac -shortest "$TEST_INPUT" 2>/dev/null
    log "测试视频生成完毕"
fi

# ── 3. Start server in background ──
log "启动 livestream 服务..."
"$PROJECT_DIR/target/release/livestream" &
SERVER_PID=$!

cleanup() {
    log "清理: 停止服务 (PID=$SERVER_PID)"
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    PUSHER_PID="${PUSHER_PID:-}"
    if [ -n "$PUSHER_PID" ]; then
        kill "$PUSHER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# ── 4. Wait for gRPC ready ──
log "等待 gRPC 就绪 (端口 $GRPC_PORT)..."
READY=0
for i in $(seq 1 30); do
    if timeout 1 bash -c "echo > /dev/tcp/127.0.0.1/$GRPC_PORT" 2>/dev/null; then
        READY=1
        break
    fi
    sleep 1
done

if [ "$READY" -eq 0 ]; then
    err "gRPC 服务在 30s 内未就绪"
    exit 1
fi
# The TCP probe only proves a socket is listening. Service identity is
# confirmed by GetServiceInfo (via connect_and_get_info) inside the test run
# below, which fails the run if the ports/identity don't match the server.
log "gRPC 服务就绪"

log "运行 E2E 测试 (duration=10s)..."
"$PROJECT_DIR/target/release/livestream-test-utils" \
  --grpc-addr "http://127.0.0.1:$GRPC_PORT" \
  --input-file "$TEST_INPUT" \
  --streams 1 \
  --duration 10 \
  --json

log "E2E 测试完成"

# ── 5. MJPEG RTSP 推流 → 服务端转码 → HTTP-FLV 拉流验证 ──
log "MJPEG 推流验证: ffmpeg 推 RTSP MJPEG → HTTP-FLV 拉流解码"
# RTSP 会话必须先经 gRPC 预创建 (ANNOUNCE 不自动建会话)。
# ffmpeg 推 MJPEG 的 SDP 为 `m=video 0 RTP/AVP 26` 且无 rtpmap。
"$PROJECT_DIR/target/release/livestream-test-utils" \
    --grpc-addr "http://127.0.0.1:$GRPC_PORT" \
    --input-file "$TEST_INPUT" \
    --streams 1 --duration 1 --protocol rtsp \
    --live-id mjtest --precreate-only || {
    err "gRPC 预创建 RTSP 会话 mjtest 失败"
    exit 1
}

# RFC 2435 needs standard Huffman tables (huffman default) and both
# quantization tables (force_duplicated_matrix), otherwise ffmpeg's own
# RTP payloader/depacketizer rejects the frames.
ffmpeg -loglevel error -re -f lavfi -i testsrc2=size=640x360:rate=10 \
    -c:v mjpeg -q:v 5 -huffman default -force_duplicated_matrix 1 \
    -f rtsp -rtsp_transport tcp \
    "rtsp://127.0.0.1:$RTSP_PORT/live/mjtest" &
PUSHER_PID=$!

MJPEG_OK=0
for i in $(seq 1 6); do
    if timeout 12 ffmpeg -v error -t 4 -i "http://127.0.0.1:$HTTP_FLV_PORT/lives/mjtest.flv" \
        -frames:v 5 -f null - 2>/tmp/mjpeg_flv_err.txt; then
        MJPEG_OK=1
        break
    fi
    sleep 2
done
if [ "$MJPEG_OK" -eq 0 ]; then
    err "MJPEG→H.264 转码链路验证失败"; cat /tmp/mjpeg_flv_err.txt >&2 || true
    kill "$PUSHER_PID" 2>/dev/null || true
    exit 1
fi
log "MJPEG 验证通过: HTTP-FLV 输出可解码 (H.264)"
kill "$PUSHER_PID" 2>/dev/null || true
wait "$PUSHER_PID" 2>/dev/null || true
