#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <signed-app> <output-dmg>" >&2
  exit 64
fi
APP="$1"
DMG="$2"
[[ -d "$APP" ]] || { echo "app not found: $APP" >&2; exit 66; }
/usr/bin/codesign --verify --deep --strict "$APP" || {
  echo "app must have a valid code signature" >&2
  exit 65
}
SIGNATURE="$(/usr/bin/codesign -dvv "$APP" 2>&1 || true)"
printf '%s\n' "$SIGNATURE" | /usr/bin/grep -Fq 'Authority=Developer ID Application:' || {
  echo "app must be signed by a Developer ID Application identity" >&2
  exit 65
}
BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist")"
[[ "$BUNDLE_ID" == "com.octet.serve.macos" ]] || {
  echo "unexpected bundle identifier: $BUNDLE_ID" >&2
  exit 65
}
/usr/bin/xcrun stapler validate "$APP" || {
  echo "app must have a valid stapled notarization ticket" >&2
  exit 65
}
/usr/sbin/spctl --assess --type execute --verbose=2 "$APP" || {
  echo "app must pass Gatekeeper assessment" >&2
  exit 65
}
if [[ -e "$DMG" ]]; then
  [[ "${CONFIRM_DMG:-}" == "OCTET_DMG" ]] || {
    echo "Set CONFIRM_DMG=OCTET_DMG to replace $DMG." >&2
    exit 77
  }
fi
OUTPUT_DIR="$(dirname "$DMG")"
mkdir -p "$OUTPUT_DIR"
STAGING="$(mktemp -d "$OUTPUT_DIR/.octet-dmg.XXXXXX")"
TEMP_DMG="$STAGING/Octet Serve.dmg"
BACKUP="$OUTPUT_DIR/.$(basename "$DMG").previous.$$"
trap 'rm -rf "$STAGING"' EXIT
APP_STAGING="$STAGING/payload"
mkdir -p "$APP_STAGING"
/usr/bin/ditto "$APP" "$APP_STAGING/Octet Serve.app"
ln -s /Applications "$APP_STAGING/Applications"
/usr/bin/hdiutil create -volname "Octet Serve" -srcfolder "$APP_STAGING" -ov -format UDZO "$TEMP_DMG"

if [[ -e "$DMG" ]]; then
  [[ ! -e "$BACKUP" ]] || { echo "stale DMG backup exists: $BACKUP" >&2; exit 75; }
  /usr/bin/mv "$DMG" "$BACKUP"
fi
if ! /usr/bin/mv "$TEMP_DMG" "$DMG"; then
  if [[ -e "$BACKUP" ]]; then /usr/bin/mv "$BACKUP" "$DMG"; fi
  exit 1
fi
if [[ -e "$BACKUP" ]]; then /usr/bin/rm -f "$BACKUP"; fi
printf 'Created %s\n' "$DMG"
