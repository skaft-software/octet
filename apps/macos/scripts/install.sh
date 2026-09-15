#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <signed-app>" >&2
  exit 64
fi
APP="$1"
[[ -d "$APP" ]] || { echo "app not found: $APP" >&2; exit 66; }
[[ "${CONFIRM_INSTALL:-}" == "OCTET_INSTALL" ]] || {
  echo "Set CONFIRM_INSTALL=OCTET_INSTALL to install into /Applications." >&2
  exit 77
}
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

TARGET="/Applications/Octet Serve.app"
PARENT="$(dirname "$TARGET")"
STAGING="$(mktemp -d "$PARENT/.octet-install.XXXXXX")"
BACKUP="$PARENT/.Octet Serve.app.previous.$$"
trap 'rm -rf "$STAGING"' EXIT
/usr/bin/ditto "$APP" "$STAGING/Octet Serve.app"
if [[ -e "$TARGET" ]]; then
  [[ ! -e "$BACKUP" ]] || { echo "stale install backup exists: $BACKUP" >&2; exit 75; }
  /usr/bin/mv "$TARGET" "$BACKUP"
fi
if ! /usr/bin/mv "$STAGING/Octet Serve.app" "$TARGET"; then
  if [[ -e "$BACKUP" ]]; then /usr/bin/mv "$BACKUP" "$TARGET"; fi
  exit 1
fi
if [[ -e "$BACKUP" ]]; then /usr/bin/rm -rf "$BACKUP"; fi
printf 'Installed %s\n' "$TARGET"
