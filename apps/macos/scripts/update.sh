#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 <artifact-app> <expected-sha256> <installed-app>" >&2
  exit 64
fi
ARTIFACT="$1"
EXPECTED="$2"
TARGET="$3"

[[ -d "$ARTIFACT" ]] || { echo "artifact not found" >&2; exit 66; }
[[ "$EXPECTED" =~ ^[[:xdigit:]]{64}$ ]] || { echo "expected SHA-256 required" >&2; exit 64; }
[[ "${CONFIRM_UPDATE:-}" == "OCTET_UPDATE" ]] || {
  echo "Set CONFIRM_UPDATE=OCTET_UPDATE to replace $TARGET." >&2
  exit 77
}
/usr/bin/codesign --verify --deep --strict "$ARTIFACT" || {
  echo "artifact must have a valid code signature" >&2
  exit 65
}
SIGNATURE="$(/usr/bin/codesign -dvv "$ARTIFACT" 2>&1 || true)"
printf '%s\n' "$SIGNATURE" | /usr/bin/grep -Fq 'Authority=Developer ID Application:' || {
  echo "artifact must be signed by a Developer ID Application identity" >&2
  exit 65
}
BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$ARTIFACT/Contents/Info.plist")"
[[ "$BUNDLE_ID" == "com.octet.serve.macos" ]] || {
  echo "unexpected bundle identifier: $BUNDLE_ID" >&2
  exit 65
}
ACTUAL="$(ditto -c -k --sequesterRsrc --keepParent "$ARTIFACT" - | shasum -a 256 | awk '{print $1}')"
ACTUAL="$(printf '%s' "$ACTUAL" | tr '[:upper:]' '[:lower:]')"
EXPECTED_NORMALIZED="$(printf '%s' "$EXPECTED" | tr '[:upper:]' '[:lower:]')"
[[ "$ACTUAL" == "$EXPECTED_NORMALIZED" ]] || { echo "artifact hash mismatch" >&2; exit 65; }

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$ARTIFACT/Contents/Info.plist")"
CURRENT="0"
if [[ -f "$TARGET/Contents/Info.plist" ]]; then
  CURRENT="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$TARGET/Contents/Info.plist" 2>/dev/null || echo 0)"
fi
[[ "$VERSION" != "$CURRENT" ]] || { echo "artifact version is already installed" >&2; exit 75; }

PARENT="$(dirname "$TARGET")"
STAGING="$(mktemp -d "$PARENT/.octet-update.XXXXXX")"
BACKUP="$PARENT/.Octet Serve.app.previous.$$"
trap 'rm -rf "$STAGING"' EXIT
/usr/bin/ditto "$ARTIFACT" "$STAGING/Octet Serve.app"
if [[ -e "$TARGET" ]]; then
  [[ ! -e "$BACKUP" ]] || { echo "stale update backup exists: $BACKUP" >&2; exit 75; }
  /usr/bin/mv "$TARGET" "$BACKUP"
fi
if ! /usr/bin/mv "$STAGING/Octet Serve.app" "$TARGET"; then
  if [[ -e "$BACKUP" ]]; then /usr/bin/mv "$BACKUP" "$TARGET"; fi
  exit 1
fi
if [[ -e "$BACKUP" ]]; then /usr/bin/rm -rf "$BACKUP"; fi
printf 'Updated %s from %s to %s\n' "$TARGET" "$CURRENT" "$VERSION"
