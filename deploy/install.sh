#!/usr/bin/env bash
# DivertLink Linux 一键安装脚本（gateway / agent / 两者）
#
# 功能：下载（或从本地文件安装）release 二进制 -> 创建专用系统用户 ->
#       创建配置/证书/状态目录 -> 用 `divertlink init` 生成配置骨架与自签证书 ->
#       安装并启用 systemd 单元。
#
# 幂等行为：重复执行只升级二进制并刷新 systemd 单元；已有配置默认保留，
#           需要重新生成时显式加 --force-config（会重新生成配置与自签证书）。
#
# 示例：
#   sudo ./deploy/install.sh --server --version 0.1.0 --domain llm.example.com
#   sudo ./deploy/install.sh --agent  --version 0.1.0 --gateway llm.example.com:443 --token 'agt_xxx'
#   sudo ./deploy/install.sh --server --binary ./divertlink-0.1.0-x86_64-unknown-linux-musl.tar.gz
#
# 前提：Linux + systemd + root 权限；release 资产命名见 .github/workflows/release.yml。
set -euo pipefail

readonly GITHUB_REPOSITORY="divertlink/divertlink"
readonly RELEASE_DOWNLOAD_BASE="https://github.com/${GITHUB_REPOSITORY}/releases/download"
readonly LATEST_RELEASE_API="https://api.github.com/repos/${GITHUB_REPOSITORY}/releases/latest"
UNIT_DIR_DEFAULT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/systemd"
readonly UNIT_DIR_DEFAULT

DEFAULT_PREFIX="/usr/local"
DEFAULT_CONFIG_DIR="/etc/divertlink"
DEFAULT_STATE_DIR="/var/lib/divertlink"
DEFAULT_USER="divertlink"

ROLE=""
VERSION=""
BINARY_SPEC=""
PREFIX="$DEFAULT_PREFIX"
CONFIG_DIR="$DEFAULT_CONFIG_DIR"
STATE_DIR="$DEFAULT_STATE_DIR"
SVC_USER="$DEFAULT_USER"
DOMAIN=""
GATEWAY=""
TOKEN=""
UNIT_DIR="$UNIT_DIR_DEFAULT"
NO_START=0
FORCE_CONFIG=0
DRY_RUN=0

# 运行时决定是否下载 release
TARGET=""
STAGED_BIN=""
WORK_DIR=""

log() { printf '[divertlink-install] %s\n' "$*"; }
warn() { printf '[divertlink-install] WARNING: %s\n' "$*" >&2; }
die() {
    printf '[divertlink-install] ERROR: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
DivertLink Linux 安装脚本

用法：sudo ./deploy/install.sh --server|--agent|--both [选项]

必选（三选一）：
  --server               安装 gateway（公网服务器）
  --agent                安装 agent（家庭侧客户端）
  --both                 同机同时安装 gateway 与 agent（测试/单机整合场景）

选项：
  --version <x.y.z>      指定 release 版本（默认查询 GitHub 最新 release）
  --binary <路径>        离线安装：本地 release tar.gz 或已解压的 divertlink 二进制
  --prefix <目录>        二进制安装前缀，默认 /usr/local（最终为 <prefix>/bin/divertlink）
  --config-dir <目录>    配置目录，默认 /etc/divertlink
  --state-dir <目录>     运行时状态目录，默认 /var/lib/divertlink
  --user <用户名>        systemd 服务用户，默认 divertlink
  --domain <域名>        server：写入证书 SAN 与下一步提示；agent：未给 --gateway 时作为默认网关
  --gateway <host[:port]>
                         agent 连接的服务端地址，默认取 --domain 的 443 端口
  --token <token>        预置隧道 token（两端必须一致）；缺省时自动生成随机 token
  --unit-dir <目录>      systemd 单元模板目录，默认脚本旁的 systemd/
  --no-start             只安装并启用开机自启，不立即启动/重启服务
  --force-config         覆盖已存在的配置骨架（会重新生成配置与自签证书）
  --dry-run              只打印计划，不修改系统
  -h, --help             显示帮助

安装完成后：
  systemctl status divertlink-server.service    # 或 divertlink-agent.service
  journalctl -u divertlink-server.service -f
EOF
}

want_server() { [[ "$ROLE" == "server" || "$ROLE" == "both" ]]; }
want_agent() { [[ "$ROLE" == "agent" || "$ROLE" == "both" ]]; }

require_root() {
    if [[ "${EUID}" -ne 0 ]]; then
        die "需要 root 权限；请使用 sudo 运行本脚本"
    fi
}

normalize_gateway() {
    local gateway="$1"
    gateway="${gateway#https://}"
    gateway="${gateway#tls://}"
    gateway="${gateway%/}"
    if [[ "$gateway" != *:* ]]; then
        gateway="${gateway}:443"
    fi
    printf '%s' "$gateway"
}

validate_inputs() {
    if [[ -z "$ROLE" ]]; then
        die "必须指定 --server、--agent 或 --both（--help 查看用法）"
    fi
    if [[ "$PREFIX" != /* || "$CONFIG_DIR" != /* || "$STATE_DIR" != /* ]]; then
        die "--prefix、--config-dir、--state-dir 必须是绝对路径"
    fi
    case "$PREFIX$CONFIG_DIR$STATE_DIR" in
        *[\&\|\\]* | *'<'* | *'>'*) die "路径中不能包含 & | \\ < > 等 sed 特殊字符" ;;
    esac
    if [[ ! "$SVC_USER" =~ ^[a-z_][a-z0-9_-]*$ ]]; then
        die "--user 只允许小写字母、数字、下划线与连字符"
    fi
    if [[ ! "$UNIT_DIR" = /* ]]; then
        UNIT_DIR="$(cd -- "$UNIT_DIR" 2>/dev/null && pwd)" || die "--unit-dir 不存在：$UNIT_DIR"
    fi
    if [[ -n "$DOMAIN" ]]; then
        if [[ ! "$DOMAIN" =~ ^[A-Za-z0-9._-]+$ ]]; then
            die "--domain 只能是域名或 IP（不允许端口、路径、通配符）"
        fi
    fi
    if [[ -n "$VERSION" ]]; then
        VERSION="${VERSION#v}"
        if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
            die "--version 需要形如 0.1.0 的版本号"
        fi
    fi
    if [[ -n "$TOKEN" && ! "$TOKEN" =~ ^[A-Za-z0-9._-]+$ ]]; then
        die "--token 只允许字母、数字、点、下划线与连字符"
    fi
    if [[ "$ROLE" == "server" && -n "$GATEWAY" ]]; then
        die "--gateway 仅用于 --agent / --both 安装"
    fi
    if want_agent; then
        if [[ -n "$GATEWAY" ]]; then
            GATEWAY="$(normalize_gateway "$GATEWAY")"
        elif [[ -n "$DOMAIN" ]]; then
            GATEWAY="$(normalize_gateway "$DOMAIN")"
        elif [[ ! -f "${CONFIG_DIR}/agent.toml" ]] || (( FORCE_CONFIG )); then
            die "agent 首次安装（或 --force-config）需要 --gateway <host[:port]>（或 --domain <域名>）"
        fi
        # 只拦截会破坏 sed 替换/TOML 字符串的字符；主机名合法性由后续 check 兜底
        if [[ "$GATEWAY" == *'&'* || "$GATEWAY" == *'|'* || "$GATEWAY" == *'"'* \
            || "$GATEWAY" == *"'"* || "$GATEWAY" == *\\* || "$GATEWAY" == *' '* ]] \
            || [[ "$GATEWAY" == *$'\t'* || "$GATEWAY" == *$'\n'* ]]; then
            die "--gateway 含非法字符：$GATEWAY"
        fi
    fi
    if [[ -n "$BINARY_SPEC" && ! -f "$BINARY_SPEC" ]]; then
        die "--binary 文件不存在：$BINARY_SPEC"
    fi
}

detect_target() {
    local machine
    machine="$(uname -m)"
    case "$machine" in
        x86_64 | amd64) TARGET="x86_64-unknown-linux-musl" ;;
        aarch64 | arm64) TARGET="aarch64-unknown-linux-musl" ;;
        *) die "不支持的 CPU 架构：${machine}（仅提供 x86_64 / aarch64 静态 musl 包）" ;;
    esac
}

download() {
    local url="$1"
    local destination="$2"
    if command -v curl >/dev/null 2>&1; then
        curl --fail --silent --show-error --location --retry 3 --connect-timeout 15 \
            --output "$destination" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget --quiet --timeout=15 --tries=3 --output-document="$destination" "$url"
    else
        die "需要 curl 或 wget 才能下载 release；离线安装请使用 --binary"
    fi
}

fetch_latest_version() {
    local tmp_file
    tmp_file="$(mktemp "${WORK_DIR}/latest.XXXXXX")"
    log "查询最新 release 版本…" >&2
    if ! download "$LATEST_RELEASE_API" "$tmp_file"; then
        die "无法查询最新版本；请显式传入 --version，或使用 --binary 离线安装"
    fi
    local tag
    tag="$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$tmp_file")"
    tag="${tag%%$'\n'*}"
    rm -f "$tmp_file"
    if [[ -z "$tag" ]]; then
        die "无法解析 release 版本；请显式传入 --version"
    fi
    VERSION="${tag#v}"
    if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
        die "release 版本号异常：$tag"
    fi
}

verify_sha256() {
    local archive="$1"
    local checksum_file="$2"
    local expected
    local actual
    expected="$(awk 'NR == 1 { print $1 }' "$checksum_file")"
    if [[ "${#expected}" -ne 64 ]]; then
        die "SHA256 校验文件格式异常：$checksum_file"
    fi
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$archive" | awk '{ print $1 }')"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$archive" | awk '{ print $1 }')"
    else
        die "缺少 sha256sum/shasum，无法校验下载文件"
    fi
    if [[ "$actual" != "$expected" ]]; then
        die "SHA256 校验失败：${archive}（期望 ${expected}，实际 ${actual}）"
    fi
    log "SHA256 校验通过"
}

# 从 tar.gz 或裸二进制准备可执行文件，结果写入全局 STAGED_BIN。
prepare_binary() {
    local source="$1"
    local extracted=""
    local candidate=""
    if [[ "$source" == *.tar.gz || "$source" == *.tgz ]] \
        || tar -tzf "$source" >/dev/null 2>&1; then
        tar -xzf "$source" -C "$WORK_DIR"
        while IFS= read -r candidate; do
            break
        done < <(find "$WORK_DIR" -type f -name 'divertlink' -perm -u+x 2>/dev/null)
        if [[ -z "$candidate" ]]; then
            while IFS= read -r candidate; do
                break
            done < <(find "$WORK_DIR" -type f -name 'divertlink' 2>/dev/null)
        fi
        if [[ -z "$candidate" ]]; then
            die "归档中未找到 divertlink 二进制：$source"
        fi
        extracted="$candidate"
    else
        extracted="$source"
    fi
    chmod 0755 "$extracted"
    if ! "$extracted" version >/dev/null 2>&1; then
        die "二进制无法在本机执行（架构不匹配或文件损坏）：$extracted"
    fi
    STAGED_BIN="$extracted"
}

# 下载并校验 release 资产，结果写入全局 STAGED_BIN。
download_release() {
    local archive_name
    local archive_path
    local checksum_path
    if [[ -z "$VERSION" ]]; then
        fetch_latest_version
    fi
    archive_name="divertlink-${VERSION}-${TARGET}.tar.gz"
    archive_path="${WORK_DIR}/${archive_name}"
    checksum_path="${archive_path}.sha256"
    log "下载 ${archive_name}…"
    download "${RELEASE_DOWNLOAD_BASE}/v${VERSION}/${archive_name}" "$archive_path"
    download "${RELEASE_DOWNLOAD_BASE}/v${VERSION}/${archive_name}.sha256" "$checksum_path"
    verify_sha256 "$archive_path" "$checksum_path"
    prepare_binary "$archive_path"
}

group_exists() {
    local name="$1"
    if command -v getent >/dev/null 2>&1; then
        getent group "$name" >/dev/null 2>&1
    else
        grep -q "^${name}:" /etc/group
    fi
}

ensure_service_user() {
    if id -u "$SVC_USER" >/dev/null 2>&1; then
        log "服务用户已存在：$SVC_USER"
        return 0
    fi
    local nologin="/usr/sbin/nologin"
    if [[ -x /sbin/nologin ]]; then
        nologin="/sbin/nologin"
    fi
    if ! group_exists "$SVC_USER"; then
        command -v groupadd >/dev/null 2>&1 || die "缺少 groupadd，无法创建服务用户"
        groupadd --system "$SVC_USER"
    fi
    command -v useradd >/dev/null 2>&1 || die "缺少 useradd，无法创建服务用户"
    useradd --system \
        --gid "$SVC_USER" \
        --home-dir "$STATE_DIR" \
        --no-create-home \
        --shell "$nologin" \
        --comment "DivertLink service account" \
        "$SVC_USER"
    log "已创建服务用户：$SVC_USER"
}

ensure_dir() {
    local dir="$1"
    local mode="$2"
    local owner="$3"
    local group="$4"
    if [[ ! -d "$dir" ]]; then
        install -d -m "$mode" -o "$owner" -g "$group" "$dir"
    else
        chmod "$mode" "$dir"
        chown "$owner:$group" "$dir"
    fi
}

install_binary() {
    local destination_dir="${PREFIX}/bin"
    ensure_dir "$destination_dir" 0755 root root
    install -m 0755 -o root -g root "$STAGED_BIN" "${destination_dir}/divertlink"
    log "二进制已安装：${destination_dir}/divertlink"
}

random_hex() {
    local bytes="$1"
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex "$bytes"
    else
        od -An -N "$bytes" -tx1 /dev/urandom | tr -d ' \n'
    fi
}

# ------------------------------------------------------------------------------
# 配置骨架：统一用 `divertlink init` 生成（保证与程序内置示例一致、能通过 check），
# 再用 sed 写入脚本侧的 token / 网关地址。
# ------------------------------------------------------------------------------

patch_token() {
    local file="$1"
    local token="$2"
    sed -i "s|^token = .*|token = \"${token}\"|" "$file"
}

patch_agent_gateway() {
    local file="$1"
    local gateway="$2"
    sed -i "s|^server = .*|server = \"${gateway}\"|" "$file"
}

# 只允许 root 与 divertlink 组读配置（内含 token / API Key）
secure_config_file() {
    local file="$1"
    [[ -f "$file" ]] || return 0
    chown root:"$SVC_USER" "$file"
    chmod 0640 "$file"
}

# 证书：服务用户只需读自己那份；CA 私钥仅 root 可读（服务运行时不使用）。
secure_cert_files() {
    [[ -d "${CONFIG_DIR}/certs" ]] || return 0
    local f
    local base
    for f in "${CONFIG_DIR}/certs"/*; do
        [[ -f "$f" ]] || continue
        base="$(basename "$f")"
        case "$base" in
            ca.key)
                chown root:root "$f"
                chmod 0600 "$f"
                ;;
            *.key)
                chown root:"$SVC_USER" "$f"
                chmod 0640 "$f"
                ;;
            *)
                chown root:"$SVC_USER" "$f"
                chmod 0644 "$f"
                ;;
        esac
    done
}

verify_config() {
    local file="$1"
    if ! "$STAGED_BIN" check -c "$file" >/dev/null; then
        die "生成的配置无法通过校验：${file}（请附上以上报错信息反馈）"
    fi
    log "配置校验通过：$file"
}

# 结果写入全局 TOKEN / API_KEY，以及 created_server / created_agent。
generate_configs() {
    local force_arg=""
    if (( FORCE_CONFIG )); then
        force_arg="--force"
    fi
    created_server=0
    created_agent=0
    TOKEN_OVERRIDE="$TOKEN"
    TOKEN=""
    API_KEY=""

    if want_server && { [[ ! -f "${CONFIG_DIR}/server.toml" ]] || (( FORCE_CONFIG )); }; then
        local domain="${DOMAIN:-localhost}"
        log "生成 gateway 配置骨架与自签证书：${CONFIG_DIR}/server.toml（domain=${domain}）"
        # shellcheck disable=SC2086 # force_arg 是受控的单个词
        "$STAGED_BIN" init --role server --dir "$CONFIG_DIR" --domain "$domain" $force_arg >/dev/null
        created_server=1
        TOKEN="$(sed -n 's/^token = "\(.*\)"/\1/p' "${CONFIG_DIR}/server.toml" | head -n 1)"
        API_KEY="$(sed -n 's/^key = "\(dlk_.*\)"/\1/p' "${CONFIG_DIR}/server.toml" | head -n 1)"
        [[ -n "$TOKEN" ]] || die "无法从 ${CONFIG_DIR}/server.toml 解析 tunnel token"
        [[ -n "$API_KEY" ]] || die "无法从 ${CONFIG_DIR}/server.toml 解析 API key"
        if [[ -n "$TOKEN_OVERRIDE" ]]; then
            TOKEN="$TOKEN_OVERRIDE"
            patch_token "${CONFIG_DIR}/server.toml" "$TOKEN"
        fi
        verify_config "${CONFIG_DIR}/server.toml"
    fi

    if want_agent && { [[ ! -f "${CONFIG_DIR}/agent.toml" ]] || (( FORCE_CONFIG )); }; then
        [[ -n "$GATEWAY" ]] || die "缺少 --gateway；无法生成 agent 配置"
        log "生成 agent 配置骨架：${CONFIG_DIR}/agent.toml（gateway=${GATEWAY}）"
        # shellcheck disable=SC2086 # force_arg 是受控的单个词
        "$STAGED_BIN" init --role agent --dir "$CONFIG_DIR" $force_arg >/dev/null
        created_agent=1
        patch_agent_gateway "${CONFIG_DIR}/agent.toml" "$GATEWAY"
        if [[ -z "$TOKEN" ]]; then
            TOKEN="${TOKEN_OVERRIDE:-agt_$(random_hex 24)}"
        fi
        patch_token "${CONFIG_DIR}/agent.toml" "$TOKEN"
        verify_config "${CONFIG_DIR}/agent.toml"
    fi

    secure_config_file "${CONFIG_DIR}/server.toml"
    secure_config_file "${CONFIG_DIR}/agent.toml"
    secure_cert_files

    if (( created_server )) && [[ -z "$API_KEY" ]]; then
        API_KEY="$(sed -n 's/^key = "\(dlk_.*\)"/\1/p' "${CONFIG_DIR}/server.toml" | head -n 1)"
    fi
    if [[ -n "$TOKEN_OVERRIDE" && -f "${CONFIG_DIR}/server.toml" && -z "$TOKEN" ]]; then
        TOKEN="$TOKEN_OVERRIDE"
    fi
}

install_systemd_unit() {
    local role="$1"
    local unit_source="${UNIT_DIR}/divertlink-${role}.service"
    local unit_target="/etc/systemd/system/divertlink-${role}.service"
    if [[ ! -f "$unit_source" ]]; then
        die "找不到 systemd 单元模板：$unit_source
请在项目仓库（含 deploy/systemd/）中运行本脚本，或用 --unit-dir 指定模板目录。"
    fi
    sed \
        -e "s|/usr/local/bin/divertlink|${PREFIX}/bin/divertlink|g" \
        -e "s|/etc/divertlink|${CONFIG_DIR}|g" \
        -e "s|/var/lib/divertlink|${STATE_DIR}|g" \
        -e "s|^User=divertlink$|User=${SVC_USER}|" \
        -e "s|^Group=divertlink$|Group=${SVC_USER}|" \
        "$unit_source" >"$unit_target"
    chmod 0644 "$unit_target"
    log "systemd 单元已安装：$unit_target"
}

start_unit() {
    local role="$1"
    local unit="divertlink-${role}.service"
    systemctl enable "$unit" >/dev/null
    if (( NO_START )); then
        log "已启用开机自启（--no-start，未立即启动）：$unit"
        return 0
    fi
    if [[ "$role" == "server" ]]; then
        if [[ ! -f "${CONFIG_DIR}/certs/server.crt" || ! -f "${CONFIG_DIR}/certs/server.key" ]]; then
            warn "证书文件尚不存在：${CONFIG_DIR}/certs/server.crt / server.key"
            warn "已启用开机自启，但本次跳过启动；放置证书（可用 certbot）后执行："
            warn "  sudo systemctl start ${unit}"
            return 0
        fi
    fi
    if systemctl restart "$unit"; then
        log "服务已启动：$unit"
    else
        warn "服务启动失败，请检查配置与证书：journalctl -u ${unit} -n 50 --no-pager"
    fi
}

print_plan() {
    log "[dry-run] 角色：$ROLE"
    log "[dry-run] 版本：${VERSION:-（自动查询最新 release）}"
    log "[dry-run] 本地文件：${BINARY_SPEC:-（联网下载）}"
    log "[dry-run] 二进制：${PREFIX}/bin/divertlink"
    log "[dry-run] 配置目录：${CONFIG_DIR}"
    log "[dry-run] 状态目录：${STATE_DIR}"
    log "[dry-run] 服务用户：${SVC_USER}"
    if want_server; then
        log "[dry-run] systemd 单元：/etc/systemd/system/divertlink-server.service"
    fi
    if want_agent; then
        log "[dry-run] systemd 单元：/etc/systemd/system/divertlink-agent.service"
    fi
    if want_agent; then
        log "[dry-run] 服务端地址：${GATEWAY:-（保留现有配置）}"
    fi
    log "[dry-run] 未对系统做任何修改"
}

main() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --server | --agent | --both)
                if [[ -n "$ROLE" && "$ROLE" != "${1#--}" ]]; then
                    die "--server / --agent / --both 只能指定一个"
                fi
                ROLE="${1#--}"
                shift
                ;;
            --version | --binary | --local-file | --prefix | --config-dir | --state-dir \
                | --user | --domain | --gateway | --token | --unit-dir)
                [[ $# -ge 2 ]] || die "$1 缺少参数值"
                case "$1" in
                    --version) VERSION="$2" ;;
                    --binary | --local-file) BINARY_SPEC="$2" ;;
                    --prefix) PREFIX="${2%/}" ;;
                    --config-dir) CONFIG_DIR="${2%/}" ;;
                    --state-dir) STATE_DIR="${2%/}" ;;
                    --user) SVC_USER="$2" ;;
                    --domain) DOMAIN="$2" ;;
                    --gateway) GATEWAY="$2" ;;
                    --token) TOKEN="$2" ;;
                    --unit-dir) UNIT_DIR="${2%/}" ;;
                esac
                shift 2
                ;;
            --no-start) NO_START=1; shift ;;
            --force-config) FORCE_CONFIG=1; shift ;;
            --dry-run) DRY_RUN=1; shift ;;
            -h | --help) usage; exit 0 ;;
            *) die "未知参数：$1（--help 查看用法）" ;;
        esac
    done

    validate_inputs

    if (( DRY_RUN )); then
        print_plan
        exit 0
    fi

    require_root
    if ! command -v systemctl >/dev/null 2>&1; then
        die "未检测到 systemd；本脚本仅支持 systemd 系统（容器/非 systemd 环境请手动部署）"
    fi
    detect_target

    WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/divertlink-install.XXXXXX")"
    cleanup() { rm -rf "$WORK_DIR"; }
    trap cleanup EXIT

    if [[ -n "$BINARY_SPEC" ]]; then
        BINARY_SPEC="$(cd -- "$(dirname -- "$BINARY_SPEC")" && pwd)/$(basename -- "$BINARY_SPEC")"
        log "离线安装：$BINARY_SPEC"
        prepare_binary "$BINARY_SPEC"
        if [[ -z "$VERSION" ]]; then
            VERSION="$("$STAGED_BIN" version 2>/dev/null | awk 'NR == 1 { print $NF }')" || true
            [[ -n "$VERSION" ]] || VERSION="unknown"
        fi
    else
        download_release
    fi

    ensure_service_user

    ensure_dir "$CONFIG_DIR" 0750 root "$SVC_USER"
    ensure_dir "${CONFIG_DIR}/certs" 0750 root "$SVC_USER"
    ensure_dir "$STATE_DIR" 0750 "$SVC_USER" "$SVC_USER"

    install_binary

    generate_configs

    if want_server; then
        install_systemd_unit "server"
    fi
    if want_agent; then
        install_systemd_unit "agent"
    fi
    systemctl daemon-reload

    if want_server; then
        start_unit "server"
    fi
    if want_agent; then
        start_unit "agent"
    fi

    echo
    log "安装完成（role=${ROLE}, version=${VERSION}, target=${TARGET}）"
    log "二进制   ：${PREFIX}/bin/divertlink"
    if [[ -f "${CONFIG_DIR}/server.toml" ]]; then
        log "配置文件 ：${CONFIG_DIR}/server.toml"
    fi
    if [[ -f "${CONFIG_DIR}/agent.toml" ]]; then
        log "配置文件 ：${CONFIG_DIR}/agent.toml"
    fi
    if want_server; then
        log "systemd  ：systemctl status divertlink-server；journalctl -u divertlink-server -f"
    fi
    if want_agent; then
        log "systemd  ：systemctl status divertlink-agent；journalctl -u divertlink-agent -f"
    fi

    if (( created_server )); then
        echo
        warn "以下凭据只显示一次，请妥善保存（配置文件权限为 0640 root:${SVC_USER}）"
        if [[ -n "$TOKEN" ]]; then
            printf '  隧道 token（agent.toml 的 [agent].token 必须一致）：%s\n' "$TOKEN"
        fi
        if [[ -n "$API_KEY" ]]; then
            printf '  入口 API Key（new-api/one-api 渠道密钥）：%s\n' "$API_KEY"
        fi
        if [[ -z "$DOMAIN" ]]; then
            warn "未指定 --domain，自签证书仅对 localhost 有效；公网部署请重新签发证书（见 docs/deployment.md）"
        fi
    elif (( created_agent )); then
        echo
        warn "请确保 agent.toml 的 [agent].token 与服务端 [[tunnels]].token 完全一致"
        if [[ -n "$TOKEN" ]]; then
            printf '  本次使用的 token：%s\n' "$TOKEN"
        fi
    fi

    if want_server && [[ -f "${CONFIG_DIR}/certs/server.crt" ]]; then
        echo
        log "下一步："
        log "  1) 公网部署建议用 Let's Encrypt 证书替换 ${CONFIG_DIR}/certs/server.crt|key"
        log "     （示例：certbot certonly --standalone -d ${DOMAIN:-<域名>}，详见 docs/deployment.md）"
        log "  2) divertlink check -c ${CONFIG_DIR}/server.toml"
        log "  3) systemctl restart divertlink-server"
    fi
    if want_agent; then
        echo
        log "提示：若网关使用自签证书，把网关的 ca.crt 复制到 ${CONFIG_DIR}/certs/ca.crt，"
        log "      并在 ${CONFIG_DIR}/agent.toml 的 [tls] 中启用 ca = \"certs/ca.crt\"（或改用 tls.pin）。"
    fi
}

main "$@"
