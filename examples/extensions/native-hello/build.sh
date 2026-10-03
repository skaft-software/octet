#!/usr/bin/env bash
# Local source build only: no install, registry, host launch or remote writes.
set -euo pipefail
here=$(cd -- "$(dirname -- "$0")" && pwd)
root=$(cd -- "$here/../../.." && pwd)
export CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$root/sdk/rust/target"}
out=${OCTET_NATIVE_BIN_DIR:-"$here/build"}
mkdir -p "$out"
out=$(cd -- "$out" && pwd)
cargo build --manifest-path "$root/sdk/rust/Cargo.toml" --offline --locked --lib --examples
cp "$CARGO_TARGET_DIR/debug/examples/native-hello" "$out/hello-rust"
cp "$CARGO_TARGET_DIR/debug/examples/native-probe" "$out/probe-rust"
# Static link needs platform system libraries used by Rust std. No dynamic SDK
# loader path is required. Windows is not qualified by this Bash recipe.
case "$(uname -s)" in
    Darwin) libs=(-lpthread -ldl -lm -liconv -framework Security -framework CoreFoundation -framework SystemConfiguration) ;;
    Linux) libs=(-lpthread -ldl -lm -lrt -lutil) ;;
    *) echo 'This build recipe supports macOS/Linux only' >&2; exit 1 ;;
esac
flags=(-Wall -Wextra -Werror -g)
if [[ ${SANITIZE:-0} == 1 ]]; then flags+=(-fsanitize=address,undefined -fno-omit-frame-pointer); fi
lib="$CARGO_TARGET_DIR/debug/liboctet_extension.a"
"${CC:-cc}" -std=c11 "${flags[@]}" -I"$root/sdk/c/include" "$here/c/main.c" "$lib" "${libs[@]}" -o "$out/hello-c"
"${CXX:-c++}" -std=c++17 "${flags[@]}" -I"$root/sdk/c/include" -I"$root/sdk/cpp/include" "$here/cpp/main.cpp" "$lib" "${libs[@]}" -o "$out/hello-cpp"
"${CC:-cc}" -std=c11 "${flags[@]}" -I"$root/sdk/c/include" "$root/sdk/c/tests/abi.c" "$lib" "${libs[@]}" -o "$out/probe-c"
"${CXX:-c++}" -std=c++17 "${flags[@]}" -I"$root/sdk/c/include" -I"$root/sdk/cpp/include" "$root/sdk/cpp/tests/probe.cpp" "$lib" "${libs[@]}" -o "$out/probe-cpp"
python3 - "$out" <<'PY'
import pathlib, sys
out = pathlib.Path(sys.argv[1])
for language in ('rust', 'c', 'cpp'):
    name = 'native-hello-' + language
    package = out / 'extensions' / name
    package.mkdir(parents=True, exist_ok=True)
    # TOML quoted strings need escaping for arbitrary local checkout paths.
    import json
    binary = json.dumps(str(out / ('hello-' + language)), ensure_ascii=False)
    (package / 'extension.toml').write_text(f'''name = "{name}"
version = "0.1.0"
api_version = "0.4"
requires_octet = "=0.8.2"
[entrypoint]
command = {binary}
[capabilities]
filesystem = "none"
process = false
network = false
[contributes]
tools = ["hello"]
''')
print(f'Built Rust/C/C++ processes and local manifests in {out}')
PY
