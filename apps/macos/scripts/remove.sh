#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 || "$1" != "--confirm REMOVE-OCTET" ]]; then
  echo "usage: $0 --confirm REMOVE-OCTET" >&2
  echo "This removes only the installed Octet Serve app; shared credentials remain managed by Octet Serve Client." >&2
  exit 77
fi
TARGET="/Applications/Octet Serve.app"
if [[ -d "$TARGET" ]]; then
  BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$TARGET/Contents/Info.plist" 2>/dev/null || true)"
  [[ "$BUNDLE_ID" == "com.octet.serve.macos" ]] || {
    echo "refusing to remove an app with an unexpected bundle identifier: $BUNDLE_ID" >&2
    exit 65
  }
  PARENT="$(dirname "$TARGET")"
  BACKUP="$PARENT/.Octet Serve.app.removing.$$"
  [[ ! -e "$BACKUP" ]] || { echo "stale removal staging path exists: $BACKUP" >&2; exit 75; }
  /usr/bin/mv "$TARGET" "$BACKUP"
  if ! /usr/bin/rm -rf "$BACKUP"; then
    if [[ -e "$BACKUP" ]]; then /usr/bin/mv "$BACKUP" "$TARGET"; fi
    exit 1
  fi
  printf 'Removed %s\n' "$TARGET"
else
  printf 'Nothing to remove at %s\n' "$TARGET"
fi
