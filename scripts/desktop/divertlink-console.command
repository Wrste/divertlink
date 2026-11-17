#!/usr/bin/env bash
# DivertLink 控制台启动器（macOS 可双击运行）。
#
# 查找顺序：
#   1. 环境变量 DIVERTLINK_BIN
#   2. PATH 中的 divertlink
#   3. 仓库内 target/release/divertlink
#   4. 仓库内 target/debug/divertlink
#
# 双击运行后会在浏览器中打开 http://127.0.0.1:17653/；
# 关闭终端窗口（或 Ctrl-C）即停止控制台和它启动的进程。

set -euo pipefail
cd "$(dirname "$0")"

find_binary() {
  if [[ -n "${DIVERTLINK_BIN:-}" && -x "${DIVERTLINK_BIN}" ]]; then
    printf '%s\n' "$DIVERTLINK_BIN"
    return 0
  fi
  if command -v divertlink >/dev/null 2>&1; then
    command -v divertlink
    return 0
  fi
  local root
  root="$(cd ../.. && pwd)"
  for candidate in "$root/target/release/divertlink" "$root/target/debug/divertlink"; do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

BIN="$(find_binary)" || {
  echo "未找到 divertlink 可执行文件。" >&2
  echo "请先运行 cargo build --release，或设置 DIVERTLINK_BIN 指向二进制。" >&2
  read -r -p "按回车退出…" _ || true
  exit 1
}

exec "$BIN" console --open
