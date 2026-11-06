#!/usr/bin/env bash
# 打包 macOS .app：生成 AppIcon.icns、写入 Info.plist、复制二进制并做 ad-hoc 签名。
#
# 用法：
#   ./scripts/bundle-macos.sh          # release 构建
#   ./scripts/bundle-macos.sh --debug  # debug 构建（验证打包流程更快）
#
# 可选环境变量：
#   MAZHA_VERSION           覆盖版本号（默认取 SPEC.md 变更记录最新版本）
#   MAZHA_CODESIGN_IDENTITY Developer ID 签名身份；缺省用 ad-hoc（"-"）
set -euo pipefail

DESKTOP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_DIR="$(cd "$DESKTOP_DIR/../.." && pwd)"
ICON_PNG="$DESKTOP_DIR/assets/appicon.png"
INFO_PLIST="$DESKTOP_DIR/macos/Info.plist"
BUILD_DIR="$DESKTOP_DIR/build/macos"
APP_NAME="码札"
APP_BUNDLE="$BUILD_DIR/$APP_NAME.app"

if [[ "${1:-}" == "--debug" ]]; then
    PROFILE="debug"
elif [[ "${1:-}" == "--release" || -z "${1:-}" ]]; then
    PROFILE="release"
else
    echo "未知参数：$1（支持 --release / --debug）" >&2
    exit 2
fi

VERSION="${MAZHA_VERSION:-}"
if [[ -z "$VERSION" ]]; then
    VERSION="$(awk -F'|' '/^\| [0-9]+\.[0-9]+\.[0-9]+ \|/ {gsub(/[[:space:]]/, "", $2); print $2; exit}' "$REPO_DIR/SPEC.md")"
fi
if [[ -z "$VERSION" ]]; then
    echo "无法从 SPEC.md 读取版本号，请设置 MAZHA_VERSION" >&2
    exit 1
fi

for tool in sips iconutil plutil codesign ditto; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "缺少 macOS 打包工具：$tool" >&2
        exit 1
    fi
done
[[ -f "$ICON_PNG" ]] || { echo "缺少图标：$ICON_PNG" >&2; exit 1; }
[[ -f "$INFO_PLIST" ]] || { echo "缺少 Info.plist：$INFO_PLIST" >&2; exit 1; }

echo "==> 构建 mazha-desktop（${PROFILE}）"
if [[ "$PROFILE" == "release" ]]; then
    cargo build --release --manifest-path "$DESKTOP_DIR/Cargo.toml"
else
    cargo build --manifest-path "$DESKTOP_DIR/Cargo.toml"
fi
BIN="$DESKTOP_DIR/target/$PROFILE/mazha-desktop"
[[ -x "$BIN" ]] || { echo "构建产物不存在：$BIN" >&2; exit 1; }

echo "==> 生成 .app（版本 ${VERSION}）"
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"

ICONSET="$BUILD_DIR/AppIcon.iconset"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
while read -r size name; do
    sips -z "$size" "$size" "$ICON_PNG" --out "$ICONSET/$name.png" >/dev/null
done <<'SIZES'
16 icon_16x16
32 icon_16x16@2x
32 icon_32x32
64 icon_32x32@2x
128 icon_128x128
256 icon_128x128@2x
256 icon_256x256
512 icon_256x256@2x
512 icon_512x512
1024 icon_512x512@2x
SIZES
iconutil -c icns "$ICONSET" -o "$APP_BUNDLE/Contents/Resources/AppIcon.icns"
rm -rf "$ICONSET"

cp "$INFO_PLIST" "$APP_BUNDLE/Contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$APP_BUNDLE/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$VERSION" "$APP_BUNDLE/Contents/Info.plist"
printf 'APPL????' > "$APP_BUNDLE/Contents/PkgInfo"

cp "$BIN" "$APP_BUNDLE/Contents/MacOS/mazha-desktop"
chmod +x "$APP_BUNDLE/Contents/MacOS/mazha-desktop"

SIGN_ID="${MAZHA_CODESIGN_IDENTITY:--}"
if [[ "$SIGN_ID" == "-" ]]; then
    echo "==> ad-hoc 签名（正式分发请设置 MAZHA_CODESIGN_IDENTITY）"
    codesign --force --deep --sign - "$APP_BUNDLE"
else
    echo "==> Developer ID 签名：$SIGN_ID"
    codesign --force --deep --options runtime --timestamp --sign "$SIGN_ID" "$APP_BUNDLE"
fi
codesign --verify --deep --strict "$APP_BUNDLE"

ZIP="$BUILD_DIR/Mazha-$VERSION-macos.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP_BUNDLE" "$ZIP"

echo "==> 完成"
echo "    $APP_BUNDLE"
echo "    $ZIP"
echo "    提示：正式对外分发还需 Developer ID 签名 + Apple 公证（notarytool + stapler）。"
