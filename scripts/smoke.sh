#!/usr/bin/env bash
# DivertLink 端到端冒烟测试（部署后自检 / 本地快速验证）。
#
# 流程：divertlink init 生成配置与自签证书 -> 修改为随机空闲端口 ->
#       启动 python mock 上游 + gateway + agent -> 用 curl 验证：
#         * OpenAI 兼容接口（非流式 JSON）
#         * SSE 流式逐块到达（无缓冲）
#         * 缺少/错误 API Key 返回 401
#         * public_paths 免认证路径
#         * 管理端点 /healthz /readyz /metrics /status
#         * 明文 http_listen 入口
# 结束时自动清理所有进程和临时目录。
#
# 依赖：bash、curl、python3（mock 上游）。Linux/macOS 均可。
#
# 用法：
#   ./scripts/smoke.sh                          # 默认用 target/debug/divertlink
#   ./scripts/smoke.sh -b /usr/local/bin/divertlink   # 部署后自检
#   ./scripts/smoke.sh -k                       # 失败时保留临时目录与日志
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
BINARY=""
KEEP=0

usage() {
    cat <<'EOF'
DivertLink 冒烟测试

用法：./scripts/smoke.sh [-b <binary>] [-k] [-h]

  -b, --binary <path>  指定 divertlink 二进制（默认依次尝试
                       target/debug/divertlink、target/release/divertlink、PATH 中的 divertlink）
  -k, --keep           测试结束（含失败）时保留临时目录与日志，便于排查
  -h, --help           显示帮助
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -b | --binary)
            [[ $# -ge 2 ]] || { echo "ERROR: $1 缺少参数" >&2; exit 2; }
            BINARY="$2"
            shift 2
            ;;
        -k | --keep) KEEP=1; shift ;;
        -h | --help) usage; exit 0 ;;
        *) echo "ERROR: 未知参数：$1" >&2; usage >&2; exit 2 ;;
    esac
done

for tool in bash curl python3; do
    command -v "$tool" >/dev/null 2>&1 || { echo "ERROR: 缺少依赖：$tool" >&2; exit 2; }
done

if [[ -z "$BINARY" ]]; then
    for candidate in "${ROOT}/target/debug/divertlink" "${ROOT}/target/release/divertlink"; do
        if [[ -x "$candidate" ]]; then BINARY="$candidate"; break; fi
    done
fi
if [[ -z "$BINARY" ]]; then
    BINARY="$(command -v divertlink || true)"
fi
if [[ -z "$BINARY" || ! -x "$BINARY" ]]; then
    echo "ERROR: 找不到可执行的 divertlink，请先 cargo build 或用 -b 指定" >&2
    exit 2
fi
BINARY="$(cd -- "$(dirname -- "$BINARY")" && pwd)/$(basename -- "$BINARY")"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/divertlink-smoke.XXXXXX")"
DL_DIR="${WORK}/config"
SERVER_PID=""
AGENT_PID=""
MOCK_PID=""
PASS=0

cleanup() {
    local pid
    for pid in "$AGENT_PID" "$SERVER_PID" "$MOCK_PID"; do
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
        fi
    done
    for pid in "$AGENT_PID" "$SERVER_PID" "$MOCK_PID"; do
        if [[ -n "$pid" ]]; then
            wait "$pid" 2>/dev/null || true
        fi
    done
    if (( KEEP )); then
        printf '[smoke] 保留临时目录：%s\n' "$WORK"
    else
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT INT TERM

dump_logs() {
    echo "---- gateway log (tail) ----" >&2
    tail -n 40 "${WORK}/gateway.log" 2>/dev/null >&2 || true
    echo "---- agent log (tail) ----" >&2
    tail -n 40 "${WORK}/agent.log" 2>/dev/null >&2 || true
    echo "---- mock upstream log (tail) ----" >&2
    tail -n 20 "${WORK}/mock.log" 2>/dev/null >&2 || true
}

fail() {
    printf 'FAIL %s\n' "$*" >&2
    dump_logs
    exit 1
}

pass() { PASS=$((PASS + 1)); printf 'ok   %s\n' "$*"; }

free_port() {
    python3 - <<'PY'
import socket

sock = socket.socket()
sock.bind(("127.0.0.1", 0))
print(sock.getsockname()[1])
sock.close()
PY
}

sed_i() {
    local expr="$1"
    local file="$2"
    sed -i.bak "$expr" "$file"
    rm -f "${file}.bak"
}

wait_http() {
    local url="$1"
    local want="$2"
    local deadline="$3"
    local label="$4"
    local waited=0
    local code="000"
    while (( waited < deadline )); do
        code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 2 "$url" 2>/dev/null || true)"
        if [[ "$code" == "$want" ]]; then
            pass "$label 就绪（HTTP ${code}）"
            return 0
        fi
        sleep 0.2
        waited=$((waited + 1))
    done
    fail "$label 未在 ${deadline} 次轮询内就绪（最后 HTTP ${code}）"
}

echo "== 准备"
echo "   二进制：$BINARY"
"$BINARY" version | head -n 1

PORT_TUNNEL="$(free_port)"
PORT_HTTP="$(free_port)"
PORT_ADMIN="$(free_port)"
PORT_MOCK="$(free_port)"

echo "== 生成配置与证书"
"$BINARY" init --role both --dir "$DL_DIR" --domain localhost >/dev/null
[[ -f "${DL_DIR}/server.toml" && -f "${DL_DIR}/agent.toml" ]] || fail "init 未生成配置"
[[ -f "${DL_DIR}/certs/server.crt" && -f "${DL_DIR}/certs/ca.crt" ]] || fail "init 未生成证书"
pass "init 生成配置与自签证书"

sed_i "s|^tunnel_listen = .*|tunnel_listen = \"127.0.0.1:${PORT_TUNNEL}\"|" "${DL_DIR}/server.toml"
sed_i "s|^ingress_listen = .*|ingress_listen = \"127.0.0.1:${PORT_TUNNEL}\"|" "${DL_DIR}/server.toml"
sed_i "s|^http_listen = .*|http_listen = \"127.0.0.1:${PORT_HTTP}\"|" "${DL_DIR}/server.toml"
sed_i "s|^admin_listen = .*|admin_listen = \"127.0.0.1:${PORT_ADMIN}\"|" "${DL_DIR}/server.toml"
sed_i "s|^server = .*|server = \"127.0.0.1:${PORT_TUNNEL}\"|" "${DL_DIR}/agent.toml"
sed_i "s|^url = .*|url = \"http://127.0.0.1:${PORT_MOCK}\"|" "${DL_DIR}/agent.toml"

API_KEY="$(sed -n 's/^key = "\(dlk_.*\)"/\1/p' "${DL_DIR}/server.toml" | head -n 1)"
[[ -n "$API_KEY" ]] || fail "无法从 server.toml 解析 API Key"
CA="${DL_DIR}/certs/ca.crt"
BASE="https://127.0.0.1:${PORT_TUNNEL}"
ADMIN="http://127.0.0.1:${PORT_ADMIN}"
PLAIN="http://127.0.0.1:${PORT_HTTP}"

"$BINARY" check -c "${DL_DIR}/server.toml" >/dev/null || fail "server.toml 校验失败"
"$BINARY" check -c "${DL_DIR}/agent.toml" >/dev/null || fail "agent.toml 校验失败"
pass "生成的配置通过 divertlink check"

echo "== 启动 mock 上游 / gateway / agent"
cat >"${WORK}/mock_upstream.py" <<'PY'
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def _send_json(self, payload, status=200):
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):  # noqa: N802 - http.server API
        if self.path.startswith("/health"):
            body = b"ok"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        elif self.path.startswith("/v1/models"):
            self._send_json(
                {"object": "list", "data": [{"id": "smoke-model", "object": "model"}]}
            )
        else:
            self._send_json({"error": "not found"}, 404)

    def do_POST(self):  # noqa: N802 - http.server API
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length) if length else b"{}"
        try:
            payload = json.loads(raw or b"{}")
        except ValueError:
            payload = {}
        if not self.path.startswith("/v1/chat/completions"):
            self._send_json({"error": "not found"}, 404)
            return
        if not payload.get("stream"):
            self._send_json(
                {
                    "id": "chatcmpl-smoke",
                    "object": "chat.completion",
                    "model": payload.get("model", "smoke-model"),
                    "choices": [
                        {
                            "index": 0,
                            "message": {"role": "assistant", "content": "hello from smoke upstream"},
                            "finish_reason": "stop",
                        }
                    ],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3},
                }
            )
            return
        # SSE：每 0.4s 写一个事件，用于验证网关/agent 不缓冲
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        for index in range(3):
            chunk = {
                "id": "chatcmpl-smoke",
                "object": "chat.completion.chunk",
                "choices": [{"index": 0, "delta": {"content": f"part{index} "}}],
            }
            self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
            self.wfile.flush()
            time.sleep(0.4)
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()
        self.close_connection = True


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
PY

python3 "${WORK}/mock_upstream.py" "$PORT_MOCK" >"${WORK}/mock.log" 2>&1 &
MOCK_PID=$!
wait_http "http://127.0.0.1:${PORT_MOCK}/health" "200" 100 "mock 上游"

"$BINARY" server -c "${DL_DIR}/server.toml" >"${WORK}/gateway.log" 2>&1 &
SERVER_PID=$!
wait_http "${ADMIN}/healthz" "200" 150 "gateway 管理端点"

"$BINARY" agent -c "${DL_DIR}/agent.toml" >"${WORK}/agent.log" 2>&1 &
AGENT_PID=$!
wait_http "${ADMIN}/readyz" "200" 250 "gateway 隧道（agent 已连接）"

# 等待隧道注册完成（readyz 已确认，这里再给一次极短的窗口）
sleep 0.2

echo "== 验证接口"
AUTH=(-H "Authorization: Bearer ${API_KEY}")
JSON_HEADER=(-H "Content-Type: application/json")

# 1) OpenAI 兼容非流式请求
CODE="$(curl -sS --max-time 20 --cacert "$CA" -o "${WORK}/body.json" -w '%{http_code}' \
    -X POST "${BASE}/home/v1/chat/completions" "${AUTH[@]}" "${JSON_HEADER[@]}" \
    -d '{"model":"smoke-model","messages":[{"role":"user","content":"hi"}]}')"
[[ "$CODE" == "200" ]] || fail "非流式 chat completions 期望 200，实际 ${CODE}"
grep -q "hello from smoke upstream" "${WORK}/body.json" || fail "非流式响应缺少上游内容"
pass "OpenAI 兼容非流式请求透传（200 + 上游内容）"

# 2) 模型列表
CODE="$(curl -sS --max-time 20 --cacert "$CA" -o "${WORK}/models.json" -w '%{http_code}' \
    "${BASE}/home/v1/models" "${AUTH[@]}")"
[[ "$CODE" == "200" ]] || fail "GET /models 期望 200，实际 ${CODE}"
grep -q "smoke-model" "${WORK}/models.json" || fail "模型列表缺少 smoke-model"
pass "OpenAI 兼容 GET /v1/models（200 + smoke-model）"

# 3) SSE 流式：0.6s 时已收到部分事件、但未收完，证明逐块转发而非缓冲
: >"${WORK}/sse.txt"
curl -sS -N --max-time 30 --cacert "$CA" \
    -X POST "${BASE}/home/v1/chat/completions" "${AUTH[@]}" "${JSON_HEADER[@]}" \
    -d '{"model":"smoke-model","stream":true,"messages":[{"role":"user","content":"hi"}]}' \
    >"${WORK}/sse.txt" 2>"${WORK}/sse.err" &
SSE_PID=$!
sleep 0.6
EARLY_BYTES="$(wc -c <"${WORK}/sse.txt" | tr -d ' ')"
wait "$SSE_PID" || fail "SSE 请求失败：$(cat "${WORK}/sse.err")"
TOTAL_BYTES="$(wc -c <"${WORK}/sse.txt" | tr -d ' ')"
[[ "$EARLY_BYTES" -gt 0 ]] || fail "SSE 在 0.6s 内没有任何数据（被缓冲或链路阻塞）"
[[ "$EARLY_BYTES" -lt "$TOTAL_BYTES" ]] || fail "SSE 数据在完成前已全部到达（${EARLY_BYTES}/${TOTAL_BYTES}），疑似被整体缓冲"
grep -q "data: " "${WORK}/sse.txt" || fail "SSE 响应中没有 data: 事件"
grep -q "part0" "${WORK}/sse.txt" || fail "SSE 响应缺少上游分块内容"
pass "SSE 逐块实时到达（0.6s 时 ${EARLY_BYTES}/${TOTAL_BYTES} 字节，未整体缓冲）"

# 4) 认证：缺少 key / 错误 key 都应 401
CODE="$(curl -sS --max-time 20 --cacert "$CA" -o /dev/null -w '%{http_code}' \
    "${BASE}/home/v1/models")"
[[ "$CODE" == "401" ]] || fail "缺少 API Key 期望 401，实际 ${CODE}"
pass "缺少 API Key 返回 401"
CODE="$(curl -sS --max-time 20 --cacert "$CA" -o /dev/null -w '%{http_code}' \
    -H "Authorization: Bearer dlk_wrong_key" "${BASE}/home/v1/models")"
[[ "$CODE" == "401" ]] || fail "错误 API Key 期望 401，实际 ${CODE}"
pass "错误 API Key 返回 401"

# 5) public_paths 免认证路径（tunnels.public_paths = ["/health"]）
CODE="$(curl -sS --max-time 20 --cacert "$CA" -o "${WORK}/health.txt" -w '%{http_code}' \
    "${BASE}/home/health")"
[[ "$CODE" == "200" ]] || fail "免认证路径 /home/health 期望 200，实际 ${CODE}"
grep -q "ok" "${WORK}/health.txt" || fail "免认证路径响应内容异常"
pass "public_paths 免认证路径可访问（/home/health 200）"

# 6) x-api-key 头也接受（Anthropic SDK 风格）
CODE="$(curl -sS --max-time 20 --cacert "$CA" -o /dev/null -w '%{http_code}' \
    -H "x-api-key: ${API_KEY}" "${BASE}/home/v1/models")"
[[ "$CODE" == "200" ]] || fail "x-api-key 认证期望 200，实际 ${CODE}"
pass "x-api-key 认证可用（200）"

# 7) 管理端点
for endpoint in healthz readyz; do
    CODE="$(curl -sS --max-time 10 -o /dev/null -w '%{http_code}' "${ADMIN}/${endpoint}")"
    [[ "$CODE" == "200" ]] || fail "/${endpoint} 期望 200，实际 ${CODE}"
done
curl -sS --max-time 10 -o "${WORK}/metrics.txt" "${ADMIN}/metrics"
grep -q "divertlink" "${WORK}/metrics.txt" || fail "/metrics 输出中没有 divertlink 指标"
curl -sS --max-time 10 -o "${WORK}/status.json" "${ADMIN}/status"
grep -q '"tunnels"' "${WORK}/status.json" || fail "/status 输出缺少 tunnels 字段"
pass "管理端点 /healthz /readyz /metrics /status 正常"

# 8) 明文 http_listen 入口（nginx/Caddy 回源路径）
CODE="$(curl -sS --max-time 20 -o /dev/null -w '%{http_code}' \
    "${PLAIN}/home/v1/models" "${AUTH[@]}")"
[[ "$CODE" == "200" ]] || fail "明文入口期望 200，实际 ${CODE}"
pass "明文 http_listen 入口可用（200）"

echo
echo "冒烟测试通过：${PASS} 项检查全部成功"
