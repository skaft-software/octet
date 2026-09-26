#!/usr/bin/env bash
#
# Build and install the octet computer-use host app.
#
# The host is an AppKit application, not a CLI: the driver needs a certified
# AppKit main thread with Window Server access to draw the agent cursor, and a
# bundle to own the macOS Accessibility/Screen Recording grants. This script
# produces that bundle reproducibly from source.
#
# The app installs to /Applications/OctetComputerUse.app. It deliberately does
# NOT reuse Cua's bundle identifiers (com.trycua.driver,
# com.trycua.driver.local): sharing one would merge our TCC rows with theirs, so
# whichever app installed last would inherit permissions granted for the other.
#
# Flags:
#   --install     install into /Applications after building
#   --identity ID codesign with this identity instead of the first available one
#   --ad-hoc      sign ad-hoc (rebuilds invalidate grants; development only)
#   --debug       build the debug configuration
#   --dry-run     build only, print the destination
#
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
package_dir="$script_dir/host-app"
build_dir="$script_dir/.build-host"
app_name="OctetComputerUse.app"
app_dest="/Applications/$app_name"

install_app=0
signing_identity=""
ad_hoc=0
configuration="release"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --install) install_app=1; shift ;;
        --identity) signing_identity="${2:-}"; shift 2 ;;
        --ad-hoc) ad_hoc=1; shift ;;
        --debug) configuration="debug"; shift ;;
        --dry-run) shift ;;
        *) printf 'error: unknown flag: %s\n' "$1" >&2; exit 2 ;;
    esac
done

for command in swift codesign plutil; do
    if ! command -v "$command" >/dev/null 2>&1; then
        printf 'error: required build command is unavailable: %s\n' "$command" >&2
        exit 1
    fi
done

printf 'building the host app (%s)\n' "$configuration"
swift build \
    --package-path "$package_dir" \
    --configuration "$configuration" \
    --scratch-path "$build_dir"

binary_path="$(swift build --package-path "$package_dir" --configuration "$configuration" \
    --scratch-path "$build_dir" --show-bin-path)/OctetComputerUseHost"
if [[ ! -x "$binary_path" ]]; then
    printf 'error: expected a built binary at %s\n' "$binary_path" >&2
    exit 1
fi

# Assemble the bundle. ditto preserves the code signature on the binary, which
# cp -R would risk disturbing on some filesystems.
staged="$build_dir/$app_name"
rm -rf "$staged"
mkdir -p "$staged/Contents/MacOS" "$staged/Contents/Resources"
ditto "$binary_path" "$staged/Contents/MacOS/OctetComputerUseHost"
chmod +x "$staged/Contents/MacOS/OctetComputerUseHost"
cp "$package_dir/Resources/Info.plist" "$staged/Contents/Info.plist"

# A bundle whose Info.plist is not a readable plist has no identity, so
# LaunchServices cannot resolve it and the driver cannot attribute TCC to it.
# Fail here rather than shipping an app that silently launches nothing.
if ! plutil -lint "$staged/Contents/Info.plist" >/dev/null; then
    printf 'error: Info.plist is not a valid property list\n' >&2
    exit 1
fi
for key in CFBundleIdentifier CFBundleExecutable CFBundlePackageType; do
    if [[ -z "$(plutil -extract "$key" raw -o - "$staged/Contents/Info.plist" 2>/dev/null)" ]]; then
        printf 'error: Info.plist is missing %s\n' >&2
        exit 1
    fi
done

# Bundle a driver when one is present next to the source tree, so the installed
# host does not depend on PATH. Absence is not an error: the extension
# provisions the driver itself and the host finds it on PATH.
if [[ -n "${OCTET_CUA_DRIVER_BINARY:-}" && -x "${OCTET_CUA_DRIVER_BINARY:-}" ]]; then
    ditto "$OCTET_CUA_DRIVER_BINARY" "$staged/Contents/MacOS/cua-driver"
    chmod +x "$staged/Contents/MacOS/cua-driver"
fi

# Sign the whole bundle after assembly. Signing the binary before it is inside a
# bundle is not enough: the bundle's own seal covers Info.plist and Resources.
if [[ "$ad_hoc" == "1" ]]; then
    printf 'signing ad-hoc (grants will NOT survive a rebuild)\n'
    codesign --force --sign - --timestamp=none "$staged"
else
    if [[ -z "$signing_identity" ]]; then
        # Take the first available identity rather than hardcoding one, so this
        # works on any developer machine.
        signing_identity="$(security find-identity -v -p codesigning 2>/dev/null \
            | sed -n 's/.*"\(.*\)".*/\1/p' | head -1)"
    fi
    if [[ -z "$signing_identity" ]]; then
        printf 'error: no codesigning identity is available.\n' >&2
        printf 'Install a Developer ID or Apple Development certificate, or pass --identity.\n' >&2
        printf 'Passing --ad-hoc builds an unsigned-identity app whose grants reset on every rebuild.\n' >&2
        exit 1
    fi
    printf 'signing with: %s\n' "$signing_identity"
    codesign --force --options runtime --sign "$signing_identity" --timestamp=none "$staged"
fi

if ! codesign --verify --deep --strict "$staged"; then
    printf 'error: the staged bundle failed strict signature verification\n' >&2
    exit 1
fi

identifier="$(plutil -extract CFBundleIdentifier raw -o - "$staged/Contents/Info.plist")"
printf 'verified bundle: %s (%s)\n' "$identifier" "$app_dest"

if [[ "$install_app" == "0" ]]; then
    printf 'built only; pass --install to place it at %s\n' "$app_dest"
    exit 0
fi

# Stop any previous host so a running daemon does not hold the old bundle.
if pgrep -f "$app_dest/Contents/MacOS/OctetComputerUseHost" >/dev/null 2>&1; then
    pkill -f "$app_dest/Contents/MacOS/OctetComputerUseHost" || true
    sleep 1
fi

rm -rf "$app_dest"
ditto "$staged" "$app_dest"
printf 'installed %s\n' "$app_dest"

# LaunchServices registers a new bundle identity asynchronously, and until it
# does, `open -a OctetComputerUse` fails to resolve. Register synchronously so
# the first launch after install works.
lsregister="/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister"
if [[ -x "$lsregister" ]]; then
    "$lsregister" -f "$app_dest" >/dev/null 2>&1 || true
    printf 'registered with LaunchServices\n'
fi

cat <<EOF

next steps
  1. grant permissions (this app has its own identity, so it needs its own grant):
       open -a "System Settings" >/dev/null 2>&1 || true
       Privacy & Security > Accessibility        > add $app_dest
       Privacy & Security > Screen & System Audio Recording > add $app_dest
  2. launch the host:
       open -n -g -a "Octet Computer Use"
  3. verify:
       "$app_dest/Contents/MacOS/OctetComputerUseHost" probe
EOF
