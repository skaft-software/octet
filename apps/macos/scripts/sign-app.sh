#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <Developer ID Application identity> <path-to-app>" >&2
  exit 64
fi
IDENTITY="$1"
APP_INPUT="$2"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENTITLEMENTS="$ROOT/Resources/OctetMacOS.entitlements"

[[ -d "$APP_INPUT" ]] || { echo "app not found: $APP_INPUT" >&2; exit 66; }
[[ -f "$ENTITLEMENTS" ]] || { echo "entitlements not found" >&2; exit 66; }
[[ "$IDENTITY" != "-" && -n "$IDENTITY" ]] || { echo "an explicit Developer ID identity is required" >&2; exit 64; }
IDENTITIES="$(/usr/bin/security find-identity -v -p codesigning 2>/dev/null || true)"
MATCHING_IDENTITIES="$(printf '%s\n' "$IDENTITIES" | /usr/bin/grep -F "$IDENTITY" || true)"
printf '%s\n' "$MATCHING_IDENTITIES" | /usr/bin/grep -Fq 'Developer ID Application:' || {
  echo "the requested Developer ID Application identity is not installed: $IDENTITY" >&2
  exit 65
}
BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP_INPUT/Contents/Info.plist")"
[[ "$BUNDLE_ID" == "com.octet.serve.macos" ]] || {
  echo "unexpected bundle identifier: $BUNDLE_ID" >&2
  exit 65
}

PARENT="$(cd "$(dirname "$APP_INPUT")" && pwd)"
APP_NAME="$(basename "$APP_INPUT")"
APP="$PARENT/$APP_NAME"
STAGING="$(mktemp -d "$PARENT/.octet-sign.XXXXXX")"
STAGED_APP="$STAGING/$APP_NAME"
BACKUP="$PARENT/.$APP_NAME.signed.previous.$$"
trap 'rm -rf "$STAGING"' EXIT
/usr/bin/ditto "$APP" "$STAGED_APP"
/usr/bin/codesign --force --options runtime --timestamp --entitlements "$ENTITLEMENTS" \
  --sign "$IDENTITY" "$STAGED_APP/Contents/MacOS/OctetMacOS"
/usr/bin/codesign --force --options runtime --timestamp --entitlements "$ENTITLEMENTS" \
  --sign "$IDENTITY" "$STAGED_APP"
/usr/bin/codesign --verify --deep --strict --verbose=2 "$STAGED_APP"
SIGNATURE="$(/usr/bin/codesign -dvv "$STAGED_APP" 2>&1 || true)"
printf '%s\n' "$SIGNATURE" | /usr/bin/grep -Fq 'Authority=Developer ID Application:' || {
  echo "staged app is not signed by a Developer ID Application identity" >&2
  exit 65
}

[[ ! -e "$BACKUP" ]] || { echo "stale signing backup exists: $BACKUP" >&2; exit 75; }
/usr/bin/mv "$APP" "$BACKUP"
if ! /usr/bin/mv "$STAGED_APP" "$APP"; then
  if [[ -e "$BACKUP" ]]; then /usr/bin/mv "$BACKUP" "$APP"; fi
  exit 1
fi
/usr/bin/rm -rf "$BACKUP"
printf 'Signed and verified %s\n' "$APP"
