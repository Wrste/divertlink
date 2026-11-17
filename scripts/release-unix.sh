#!/usr/bin/env bash
# 构建可直接运行的 Unix 二进制；macOS 可使用本机钥匙串签名及 Apple 公证。
set -euo pipefail

usage() {
    cat <<'EOF'
用法：scripts/release-unix.sh <target> [--sign | --notarize] [--build-tool cargo|cross|zigbuild]

target：x86_64-unknown-linux-musl、aarch64-unknown-linux-musl、
        x86_64-apple-darwin、aarch64-apple-darwin

默认用 cargo 构建。跨平台可指定 cross 或 zigbuild，需先安装对应工具链。
--sign：macOS Developer ID 签名；--notarize：签名、生成 DMG 并提交 Apple 公证。
签名身份：MACOS_SIGN_IDENTITY；未指定时要求钥匙串中恰有一个 Developer ID Application。
公证：NOTARY_KEYCHAIN_PROFILE，或 APPLE_ID / APPLE_TEAM_ID / APPLE_APP_SPECIFIC_PASSWORD。
产物：dist/divertlink-<Cargo.toml 版本>-<target>.tar.gz、.sha256，公证模式另有 .dmg。
EOF
}

die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    usage
    exit 0
fi
[[ $# -ge 1 ]] || { usage; exit 1; }
release_target="$1"
shift
sign=0
notarize=0
build_tool=cargo
while [[ $# -gt 0 ]]; do
    case "$1" in
        --sign) sign=1; shift ;;
        --notarize) sign=1; notarize=1; shift ;;
        --build-tool)
            [[ $# -ge 2 ]] || die '--build-tool 缺少参数'
            build_tool="$2"; shift 2 ;;
        *) die "未知参数：$1" ;;
    esac
done
case "$release_target" in
    x86_64-unknown-linux-musl|aarch64-unknown-linux-musl) platform=linux ;;
    x86_64-apple-darwin|aarch64-apple-darwin) platform=macos ;;
    *) die "不支持的 target：$release_target" ;;
esac
case "$build_tool" in cargo|cross|zigbuild) ;; *) die "不支持的构建工具：$build_tool" ;; esac

release_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$release_root"
if ! command -v cargo >/dev/null 2>&1 && [[ -x .toolchain/cargo/bin/cargo ]]; then
    # shellcheck source=scripts/env.sh
    . ./scripts/env.sh
fi
command -v cargo >/dev/null 2>&1 || die '请先安装 Rust stable'
release_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
[[ -n "$release_version" ]] || die '无法读取 Cargo.toml 版本'
release_name="divertlink-${release_version}-${release_target}"
mkdir -p target dist

if (( sign )); then
    [[ "$platform" == macos && "$(uname -s)" == Darwin ]] || die '签名仅支持在 macOS 上构建 macOS 产物'
    sign_identity="${MACOS_SIGN_IDENTITY:-}"
    if [[ -z "$sign_identity" ]]; then
        identities="$(security find-identity -v -p codesigning | awk '/"Developer ID Application:/ { print $2 }')"
        [[ -n "$identities" && "$(printf '%s\n' "$identities" | wc -l | tr -d ' ')" == 1 ]] ||
            die '请通过 MACOS_SIGN_IDENTITY 指定唯一的 Developer ID Application 身份'
        sign_identity="$identities"
    fi
fi
if (( notarize )); then
    xcrun --find notarytool >/dev/null
    if [[ -n "${NOTARY_KEYCHAIN_PROFILE:-}" ]]; then
        notary_auth=(--keychain-profile "$NOTARY_KEYCHAIN_PROFILE")
    else
        [[ -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-}" && -n "${APPLE_APP_SPECIFIC_PASSWORD:-}" ]] ||
            die '缺少 Apple 公证环境变量，请参阅 docs/releasing.md'
        notary_auth=(--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD")
    fi
fi

case "$build_tool" in
    cargo) cargo build --release --locked --target "$release_target" ;;
    cross) cross build --release --locked --target "$release_target" ;;
    zigbuild) cargo zigbuild --release --locked --target "$release_target" ;;
esac

stage_dir="$(mktemp -d "$release_root/target/release-stage.XXXXXX")"
trap 'rm -rf "$stage_dir"' EXIT
package_dir="$stage_dir/$release_name"
mkdir -p "$package_dir"
cp "target/$release_target/release/divertlink" "$package_dir/divertlink"
chmod 0755 "$package_dir/divertlink"
cp LICENSE-MIT LICENSE-APACHE README.md "$package_dir/"

if [[ "$platform" == macos ]]; then
    # 与二进制相邻，解压或挂载后双击即可打开内置控制台。
    cat >"$package_dir/DivertLink Console.command" <<'EOF'
#!/bin/bash
cd -- "$(dirname -- "$0")" || exit 1
exec ./divertlink console --open
EOF
    chmod 0755 "$package_dir/DivertLink Console.command"
fi
if (( sign )); then
    codesign --force --sign "$sign_identity" --options runtime --timestamp \
        --identifier io.github.wrste.divertlink "$package_dir/divertlink"
    codesign --verify --strict --verbose=2 "$package_dir/divertlink"
fi

if (( notarize )); then
    dmg="$release_root/dist/$release_name.dmg"
    hdiutil create -ov -format UDZO -volname "DivertLink $release_version" -srcfolder "$package_dir" "$dmg"
    codesign --force --sign "$sign_identity" --timestamp "$dmg"
    codesign --verify --strict --verbose=2 "$dmg"
    # 不启用 shell tracing；凭据仅传给 notarytool，不写入任何产物。
    result="$release_root/dist/$release_name.notary.json"
    xcrun notarytool submit "$dmg" "${notary_auth[@]}" --wait --timeout 30m --output-format json >"$result"
    [[ "$(plutil -extract status raw -o - "$result")" == Accepted ]] ||
        die "Apple 公证未通过，查看 $result 并用 notarytool log 获取原因"
    xcrun stapler staple "$dmg"
    xcrun stapler validate "$dmg"
    spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
    printf 'Apple 公证通过，票据已附加到 DMG。\n'
fi

# 公证通过后才生成 tar.gz；其中的二进制与 Apple 验证的版本完全相同。
# 裸命令行二进制/tar.gz 不支持 stapler；离线验证使用带票据的 DMG。
COPYFILE_DISABLE=1 tar -C "$stage_dir" -czf "dist/$release_name.tar.gz" "$release_name"
for archive in "dist/$release_name.tar.gz" "dist/$release_name.dmg"; do
    [[ -f "$archive" ]] || continue
    (
        cd dist
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum "${archive##*/}" >"${archive##*/}.sha256"
        else
            shasum -a 256 "${archive##*/}" >"${archive##*/}.sha256"
        fi
    )
done
printf '发布产物：%s/dist/%s.*\n' "$release_root" "$release_name"
