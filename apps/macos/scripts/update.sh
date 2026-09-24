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
if [[ -e "$TARGET" ]]; then
  [[ -f "$TARGET/Contents/Info.plist" ]] || { echo "installed app has no Info.plist" >&2; exit 65; }
  CURRENT="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$TARGET/Contents/Info.plist")"
fi
# CFBundleVersion consists of up to three numeric components. Compare their
# decimal strings, not machine integers (build timestamps can exceed shell limits).
VERSION_PATTERN='^[0-9]+(\.[0-9]+){0,2}$'
[[ "$VERSION" =~ $VERSION_PATTERN && "$CURRENT" =~ $VERSION_PATTERN ]] || {
  echo "invalid artifact or installed app version" >&2
  exit 65
}
IFS=. read -r -a nextParts <<< "$VERSION"
IFS=. read -r -a currentParts <<< "$CURRENT"
comparison=0
for index in 0 1 2; do
  next="${nextParts[index]:-0}"
  installed="${currentParts[index]:-0}"
  next="${next#"${next%%[!0]*}"}"
  installed="${installed#"${installed%%[!0]*}"}"
  next="${next:-0}"
  installed="${installed:-0}"
  if (( ${#next} > ${#installed} )) || { [[ ${#next} -eq ${#installed} ]] && [[ "$next" > "$installed" ]]; }; then
    comparison=1
    break
  fi
  if [[ "$next" != "$installed" ]]; then
    comparison=-1
    break
  fi
done
[[ "$comparison" -ne 0 ]] || { echo "artifact version is already installed" >&2; exit 75; }
[[ "$comparison" -gt 0 ]] || { echo "refusing to downgrade from $CURRENT to $VERSION" >&2; exit 75; }

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
