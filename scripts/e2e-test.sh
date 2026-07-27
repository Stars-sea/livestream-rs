#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
TEST_INPUT="${TEST_INPUT:-$PROJECT_DIR/../testdata/sample.mp4}"
GRPC_PORT="${GRPC_PORT:-50051}"
RTMP_PORT="${RTMP_PORT:-11935}"
RTSP_PORT="${RTSP_PORT:-8554}"
HTTP_FLV_PORT="${HTTP_FLV_PORT:-8080}"

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
log "构建 livestream 和 test-client..."
cargo build --release -p livestream -p test-client

# ── 2. Generate test input if missing ──
if [ ! -f "$TEST_INPUT" ]; then
    log "生成测试视频: $TEST_INPUT"
    mkdir -p "$(dirname "$TEST_INPUT")"
    ffmpeg -y -f lavfi -i "testsrc=duration=30:size=1280x720:rate=30" \
           -f lavfi -i "sine=frequency=440:duration=30" \
           -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
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
}
trap cleanup EXIT

# ── 4. Wait for gRPC ready ──
log "等待 gRPC 就绪 (端口 $GRPC_PORT)..."
READY=0
for i in $(seq 1 30); do
    if grpcurl -plaintext "127.0.0.1:$GRPC_PORT" livestream.Livestream/GetServiceInfo >/dev/null 2>&1; then
        READY=1
        break
    fi
    sleep 1
done

if [ "$READY" -eq 0 ]; then
    err "gRPC 服务在 30s 内未就绪"
    exit 1
fi
log "gRPC 服务就绪"

# ── 5. Run automated test ──
log "运行 E2E 测试 (duration=10s)..."
"$PROJECT_DIR/target/release/test-client" --auto --duration 10 "$TEST_INPUT"

log "E2E 测试完成"
