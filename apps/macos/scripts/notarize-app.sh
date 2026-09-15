#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo "usage: $0 <keychain-profile> <signed-app>" >&2
    echo "       CONFIRM_NOTARIZE=OCTET_NOTARIZE $0 <keychain-profile> <signed-app>" >&2
}

if [[ $# -ne 2 ]]; then
    usage
    exit 64
fi

PROFILE="$1"
APP_INPUT="$2"

if [[ -z "$PROFILE" || "$PROFILE" == -* ]]; then
    echo "a non-empty notarytool keychain profile is required" >&2
    exit 64
fi
if [[ "${CONFIRM_NOTARIZE:-}" != "OCTET_NOTARIZE" ]]; then
    echo "refusing remote notarization without CONFIRM_NOTARIZE=OCTET_NOTARIZE" >&2
    exit 77
fi
if [[ "$APP_INPUT" != *.app || ! -d "$APP_INPUT" ]]; then
    echo "signed app bundle not found: $APP_INPUT" >&2
    exit 66
fi

APP_INPUT="$(cd "$(dirname "$APP_INPUT")" && pwd)/$(basename "$APP_INPUT")"
INFO_PLIST="$APP_INPUT/Contents/Info.plist"
BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$INFO_PLIST" 2>/dev/null || true)"
if [[ "$BUNDLE_ID" != "com.octet.serve.macos" ]]; then
    echo "unexpected bundle identifier: ${BUNDLE_ID:-<missing>}" >&2
    exit 65
fi

/usr/bin/codesign --verify --deep --strict --verbose=2 "$APP_INPUT" >/dev/null
SIGNATURE="$(/usr/bin/codesign -dvv "$APP_INPUT" 2>&1 || true)"
case "$SIGNATURE" in
    *"Authority=Developer ID Application:"*) ;;
    *)
        echo "the app must be signed by a Developer ID Application identity" >&2
        exit 65
        ;;
esac

PARENT="$(dirname "$APP_INPUT")"
NAME="$(basename "$APP_INPUT")"
STAGING_ROOT="$(/usr/bin/mktemp -d "$PARENT/.octet-notarize.XXXXXX")"
ROLLBACK_ROOT=""
cleanup() {
    if [[ -n "$STAGING_ROOT" ]]; then /bin/rm -rf "$STAGING_ROOT"; fi
    if [[ -n "$ROLLBACK_ROOT" ]]; then /bin/rm -rf "$ROLLBACK_ROOT"; fi
}
trap cleanup EXIT INT TERM

STAGED_APP="$STAGING_ROOT/$NAME"
ARCHIVE="$STAGING_ROOT/$NAME.zip"
/usr/bin/ditto "$APP_INPUT" "$STAGED_APP"
/usr/bin/ditto -c -k --keepParent "$STAGED_APP" "$ARCHIVE"

# The profile is deliberately passed to notarytool without logging credentials.
/usr/bin/xcrun notarytool submit "$ARCHIVE" --keychain-profile "$PROFILE" --wait
/usr/bin/xcrun stapler staple "$STAGED_APP"
/usr/bin/xcrun stapler validate "$STAGED_APP"
/usr/bin/codesign --verify --deep --strict --verbose=2 "$STAGED_APP" >/dev/null
STAGED_SIGNATURE="$(/usr/bin/codesign -dvv "$STAGED_APP" 2>&1 || true)"
case "$STAGED_SIGNATURE" in
    *"Authority=Developer ID Application:"*) ;;
    *)
        echo "staged notarized app lost its Developer ID signature" >&2
        exit 65
        ;;
esac

# Replace only after notarization and stapling have succeeded. Keep the old
# bundle until the move of the staged bundle has completed.
ROLLBACK_ROOT="$(/usr/bin/mktemp -d "$PARENT/.octet-notarize-rollback.XXXXXX")"
BACKUP="$ROLLBACK_ROOT/$NAME"
/bin/mv "$APP_INPUT" "$BACKUP"
if ! /bin/mv "$STAGED_APP" "$APP_INPUT"; then
    /bin/mv "$BACKUP" "$APP_INPUT" || {
        echo "replacement failed and rollback could not be restored: $APP_INPUT" >&2
        exit 1
    }
    echo "replacement failed; restored the previous app" >&2
    exit 1
fi

/bin/rm -rf "$ROLLBACK_ROOT"
ROLLBACK_ROOT=""
echo "notarized and stapled app installed at $APP_INPUT"
