#!/usr/bin/env bash
# Apple 公证 + 装订：对已用 Developer ID 签名的 .app 执行 notarytool 提交流程。
#
# 前置：
#   xcrun notarytool store-credentials "mazha-notary" \
#       --apple-id "you@example.com" --team-id "TEAMID" --password "app-specific-password"
#
# 用法：
#   MAZHA_NOTARY_PROFILE=mazha-notary ./scripts/notarize-macos.sh [码札.app]
set -euo pipefail

DESKTOP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="${1:-$DESKTOP_DIR/build/macos/码札.app}"
PROFILE="${MAZHA_NOTARY_PROFILE:-}"

[[ -d "$APP" ]] || { echo "找不到 .app：$APP" >&2; exit 1; }
if [[ -z "$PROFILE" ]]; then
    echo "请设置 MAZHA_NOTARY_PROFILE（notarytool keychain profile 名称）" >&2
    exit 1
fi

IDENTITY="$(codesign -dv --verbose=2 "$APP" 2>&1 | awk -F= '/^Authority=Developer ID Application/ {print $2; exit}')"
if [[ -z "$IDENTITY" ]]; then
    echo "该 .app 不是 Developer ID 签名；请先以 MAZHA_CODESIGN_IDENTITY 重新打包" >&2
    exit 1
fi

ZIP="$(mktemp -t mazha-notary).zip"
trap 'rm -f "$ZIP"' EXIT
ditto -c -k --keepParent "$APP" "$ZIP"

echo "==> 提交 Apple 公证（$PROFILE）"
xcrun notarytool submit "$ZIP" --keychain-profile "$PROFILE" --wait

echo "==> 装订公证票据"
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
spctl --assess --type execute --verbose=4 "$APP"

echo "==> 公证完成：$APP"
