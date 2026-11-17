#!/usr/bin/env bash
# shellcheck disable=SC2016
# 说明：下面单引号 needle 中的 $scheme / $host / $STAGED_BIN 是文件字面量，不应由 shell 展开。
# 部署资产自检：语法 + 配置项一致性 + 与 Dockerfile/docs 的路径契约。
#
# 本地：make deploy-check
# CI  ：.github/workflows/ci.yml 的 deploy-assets job 直接调用本脚本。
# 不依赖 Docker（有 docker 时额外做 compose 解析）；PyYAML 缺失时跳过 YAML 解析并提示。
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

FAILED=0
ok() { printf 'ok   %s\n' "$*"; }
bad() {
    printf 'FAIL %s\n' "$*" >&2
    FAILED=1
}
skip() { printf 'skip %s\n' "$*"; }

# 文件内必须包含某些字面量（用于守住与实现/文档一致的契约）。
require_in_file() {
    local file="$1"
    local needle="$2"
    local why="$3"
    if [[ ! -f "$file" ]]; then
        bad "$file 不存在（${why}）"
        return
    fi
    if grep -qF -- "$needle" "$file"; then
        ok "$file 包含 \"$needle\""
    else
        bad "$file 缺少 \"$needle\"（${why}）"
    fi
}


echo "== shell 语法"
for script in deploy/install.sh deploy/check-assets.sh; do
    if bash -n "$script"; then ok "bash -n $script"; else bad "bash -n $script"; fi
done
if [[ -x deploy/install.sh ]]; then ok "deploy/install.sh 可执行"; else bad "deploy/install.sh 缺少可执行位"; fi

echo "== YAML 解析"
if command -v python3 >/dev/null 2>&1 && python3 -c 'import yaml' >/dev/null 2>&1; then
    python3 - <<'PY'
import glob
import sys

import yaml

paths = sorted(glob.glob(".github/workflows/*.yml") + glob.glob(".github/workflows/*.yaml"))
paths += ["deploy/docker-compose.yml"]
failed = False
for path in paths:
    try:
        with open(path, encoding="utf-8") as handle:
            yaml.safe_load(handle)
        print(f"ok   YAML 可解析：{path}")
    except Exception as exc:  # noqa: BLE001 - 逐个文件报告
        failed = True
        print(f"FAIL YAML 解析失败：{path}: {exc}", file=sys.stderr)
sys.exit(1 if failed else 0)
PY
else
    skip "python3/PyYAML 不可用，跳过 YAML 解析"
fi

echo "== docker compose 解析"
if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    if docker compose -f deploy/docker-compose.yml config -q; then
        ok "docker compose config"
    else
        bad "docker compose config"
    fi
else
    skip "docker 不可用，跳过 compose 解析"
fi

echo "== Dockerfile / .dockerignore"
require_in_file Dockerfile "COPY assets ./assets" "src/config.rs 用 include_str! 引用 assets/*.example.toml"
require_in_file Dockerfile "cargo build --release --locked" "release 构建必须 --locked"
require_in_file Dockerfile "gcr.io/distroless/static-debian12:nonroot" "运行镜像必须 distroless + 非 root"
require_in_file Dockerfile "ENTRYPOINT [\"/usr/local/bin/divertlink\"]" "入口必须是 divertlink"
require_in_file Dockerfile "EXPOSE 443/tcp 8080/tcp 9090/tcp" "必须暴露 443/8080/9090"
require_in_file .dockerignore "!assets/**" "构建上下文必须包含 assets/"
require_in_file .dockerignore "!Cargo.lock" "构建上下文必须包含 Cargo.lock（--locked）"

echo "== systemd 单元"
require_in_file deploy/systemd/divertlink-server.service "AmbientCapabilities=CAP_NET_BIND_SERVICE" "server 需要绑定 443"
require_in_file deploy/systemd/divertlink-server.service "NoNewPrivileges=yes" "安全加固"
require_in_file deploy/systemd/divertlink-server.service "ProtectSystem=strict" "安全加固"
require_in_file deploy/systemd/divertlink-server.service "PrivateTmp=yes" "安全加固"
require_in_file deploy/systemd/divertlink-server.service \
    "ExecStart=/usr/local/bin/divertlink server -c /etc/divertlink/server.toml" "ExecStart 契约"
require_in_file deploy/systemd/divertlink-server.service "ReadWritePaths=/var/lib/divertlink" "只允许状态目录可写"
require_in_file deploy/systemd/divertlink-agent.service "NoNewPrivileges=yes" "安全加固"
require_in_file deploy/systemd/divertlink-agent.service "ProtectSystem=strict" "安全加固"
require_in_file deploy/systemd/divertlink-agent.service "PrivateTmp=yes" "安全加固"
require_in_file deploy/systemd/divertlink-agent.service \
    "ExecStart=/usr/local/bin/divertlink agent -c /etc/divertlink/agent.toml" "ExecStart 契约"

echo "== nginx / Caddy（SSE 与请求体契约）"
require_in_file deploy/nginx/ingress.conf "proxy_buffering off;" "SSE 必须关闭缓冲"
require_in_file deploy/nginx/ingress.conf "proxy_read_timeout 600s;" "长请求读超时"
require_in_file deploy/nginx/ingress.conf "client_max_body_size 64m;" "与默认 64MiB 请求体一致"
require_in_file deploy/nginx/ingress.conf 'proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;' "透传客户端 IP"
require_in_file deploy/nginx/ingress.conf 'proxy_set_header X-Forwarded-Proto $scheme;' "透传协议"
require_in_file deploy/nginx/ingress.conf 'proxy_set_header Host $host;' "透传 Host"
require_in_file deploy/Caddyfile "flush_interval -1" "SSE 必须逐写 flush"
require_in_file deploy/Caddyfile "max_size 64MB" "与默认 64MiB 请求体一致"

echo "== compose 契约"
require_in_file deploy/docker-compose.yml "127.0.0.1:9090:9090/tcp" "管理端点只发布到回环"
require_in_file deploy/docker-compose.yml "NET_BIND_SERVICE" "非 root 绑定 443"
require_in_file deploy/docker-compose.yml "/etc/divertlink/server.toml" "容器内配置路径契约"

echo "== install.sh 契约"
require_in_file deploy/install.sh "set -euo pipefail" "严格模式"
require_in_file deploy/install.sh "--both" "支持同机双角色"
require_in_file deploy/install.sh "--binary" "支持离线安装"
require_in_file deploy/install.sh '"$STAGED_BIN" init' "用 divertlink init 生成配置骨架"
require_in_file deploy/install.sh "useradd --system" "创建专用服务用户"
require_in_file deploy/install.sh "divertlink-\${role}.service" "安装 systemd 单元"

echo
if (( FAILED )); then
    echo "deploy 自检失败" >&2
    exit 1
fi
echo "deploy 自检通过"
