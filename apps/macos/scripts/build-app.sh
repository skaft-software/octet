#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PRODUCT="OctetMacOS"
CONFIGURATION="${CONFIGURATION:-release}"
OUT_DIR="${OUT_DIR:-$ROOT/.artifacts}"
APP="$OUT_DIR/Octet Serve.app"

mkdir -p "$OUT_DIR"
if [[ -e "$APP" ]]; then
  [[ "${CONFIRM_BUILD:-}" == "OCTET_BUILD" ]] || {
    echo "Set CONFIRM_BUILD=OCTET_BUILD to replace $APP." >&2
    exit 77
  }
fi

STAGING="$(mktemp -d "$OUT_DIR/.octet-build.XXXXXX")"
STAGED_APP="$STAGING/Octet Serve.app"
BACKUP="$OUT_DIR/.Octet Serve.app.previous.$$"
trap 'rm -rf "$STAGING"' EXIT
mkdir -p "$STAGED_APP/Contents/MacOS" "$STAGED_APP/Contents/Resources"

swift build --package-path "$ROOT" --configuration "$CONFIGURATION" --product "$PRODUCT"
BINARY="$(swift build --package-path "$ROOT" --configuration "$CONFIGURATION" --show-bin-path)/$PRODUCT"
install -m 0755 "$BINARY" "$STAGED_APP/Contents/MacOS/$PRODUCT"
install -m 0644 "$ROOT/Sources/OctetMacOS/Resources/Info.plist" "$STAGED_APP/Contents/Info.plist"

# Keep signing separate so an unsigned source build cannot be mistaken for a
# distributable artifact. sign-app.sh is the only script that invokes codesign.
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $(date +%Y%m%d%H%M%S)" "$STAGED_APP/Contents/Info.plist"

if [[ -e "$APP" ]]; then
  [[ ! -e "$BACKUP" ]] || { echo "stale build backup exists: $BACKUP" >&2; exit 75; }
  /usr/bin/mv "$APP" "$BACKUP"
fi
if ! /usr/bin/mv "$STAGED_APP" "$APP"; then
  if [[ -e "$BACKUP" ]]; then /usr/bin/mv "$BACKUP" "$APP"; fi
  exit 1
fi
if [[ -e "$BACKUP" ]]; then /usr/bin/rm -rf "$BACKUP"; fi
printf 'Unsigned app assembled at %s\n' "$APP"
