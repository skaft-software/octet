#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 || $# -gt 5 ]]; then
    printf 'usage: %s ID OUTPUT_DIRECTORY VERSION [SOURCE_DIRECTORY [TARGET]]\n' "$0" >&2
    exit 2
fi

package_id=$1
output_directory=$2
version=$3
source_override=${4:-}
target=${5:-}
if [[ "$package_id" == octet-codemode ]]; then
    case "$target" in
        x86_64-unknown-linux-gnu|x86_64-apple-darwin|aarch64-apple-darwin|x86_64-pc-windows-gnu|x86_64-pc-windows-msvc) ;;
        *) printf 'codemode release packaging requires a supported TARGET\n' >&2; exit 2 ;;
    esac
fi

if [[ ! "$package_id" =~ ^[a-z][a-z0-9-]{0,63}$ ]]; then
    printf 'invalid extension bundle ID: %s\n' "$package_id" >&2
    exit 2
fi
if [[ ! "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$ ]]; then
    printf 'version must be a canonical octet version tag such as v0.7.0: %s\n' "$version" >&2
    exit 2
fi
for command in git python3; do
    if ! command -v "$command" >/dev/null 2>&1; then
        printf 'required release command is unavailable: %s\n' "$command" >&2
        exit 1
    fi
done

script_directory=$(cd "$(dirname "$0")" && pwd)
repository_directory=$(cd "$script_directory/.." && pwd)
package_version=${version#v}
tracked_manifest=
dependency_directory=
trap '[[ -z "$tracked_manifest" ]] || rm -f "$tracked_manifest"; [[ -z "$dependency_directory" ]] || rm -rf "$dependency_directory"' EXIT

if [[ -z "$source_override" ]]; then
    if ! grep -Ev '^[[:space:]]*(#|$)' "$repository_directory/extensions/release-catalog.txt" \
        | grep -Fx "$package_id" >/dev/null; then
        printf 'extension is not in the official release catalog: %s\n' "$package_id" >&2
        exit 2
    fi
    if ! git -C "$repository_directory" diff-index --quiet HEAD --; then
        printf 'release source has tracked changes; package an immutable clean commit\n' >&2
        exit 1
    fi
    source_directory="$repository_directory/extensions/$package_id"
    tracked_manifest=$(mktemp "${TMPDIR:-/tmp}/octet-extension-files.XXXXXX")
    git -C "$repository_directory" ls-files -z -- "extensions/$package_id" >"$tracked_manifest"
    if [[ ! -s "$tracked_manifest" ]]; then
        printf 'official extension has no tracked source files: %s\n' "$package_id" >&2
        exit 1
    fi
else
    source_directory=$(cd "$source_override" 2>/dev/null && pwd) || {
        printf 'cannot resolve extension source directory: %s\n' "$source_override" >&2
        exit 1
    }
fi

if [[ ! -d "$source_directory" || -L "$source_directory" ]]; then
    printf 'extension source is missing, linked, or not a directory: %s\n' "$source_directory" >&2
    exit 1
fi
if [[ ! -f "$source_directory/extension.toml" || -L "$source_directory/extension.toml" ]]; then
    printf 'extension source has no regular extension.toml: %s\n' "$source_directory" >&2
    exit 1
fi
if [[ -e "$source_directory/package.toml" || -e "$source_directory/install.json" ]]; then
    printf 'executable-extension sources cannot contain application/install manifests\n' >&2
    exit 1
fi

source_date_epoch=${SOURCE_DATE_EPOCH:-$(git -C "$repository_directory" log -1 --format=%ct HEAD)}
case "$source_date_epoch" in
    ''|*[!0-9]*)
        printf 'SOURCE_DATE_EPOCH must be an unsigned integer: %s\n' "$source_date_epoch" >&2
        exit 1
        ;;
esac

# Resolve the locked adapter dependencies only in private build staging. Never
# reuse a workstation node_modules tree or run npm/extension lifecycle scripts.
if [[ "$package_id" == octet-pi-compat ]]; then
    dependency_directory=$(mktemp -d "${TMPDIR:-/tmp}/octet-extension-dependencies.XXXXXX")
fi

mkdir -p "$output_directory"
archive="$output_directory/$package_id-$package_version.tar.gz"
if [[ "$package_id" == octet-codemode ]]; then
    archive="$output_directory/$package_id-$package_version-$target.tar.gz"
fi
python3 - \
    "$repository_directory" \
    "$source_directory" \
    "$package_id" \
    "$package_version" \
    "$archive" \
    "$source_date_epoch" \
    "$tracked_manifest" \
    "$dependency_directory" \
    "$target" <<'PY'
import gzip
import json
import os
import pathlib
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import tomllib

repository, source, package_id, octet_version, archive, epoch, tracked_manifest, dependency_directory, target = sys.argv[1:10]
repository = pathlib.Path(repository)
source = pathlib.Path(source)
archive = pathlib.Path(archive)
epoch = int(epoch)

with (source / "extension.toml").open("rb") as handle:
    manifest = tomllib.load(handle)
for field in ("name", "version", "api_version", "requires_octet", "entrypoint"):
    if field not in manifest:
        raise SystemExit(f"extension.toml is missing {field}")
if manifest["name"] != package_id:
    raise SystemExit(
        f"extension.toml name {manifest['name']!r} does not match {package_id!r}"
    )
if not isinstance(manifest["version"], str) or re.fullmatch(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:[-+][0-9A-Za-z.-]+)?",
    manifest["version"],
) is None:
    raise SystemExit("extension.toml version is not semantic versioning")
if manifest["api_version"] not in ("0.2", "0.3", "0.4"):
    raise SystemExit("release bundles must declare api_version = '0.2', '0.3', or '0.4'")
expected_octet = f"={octet_version}"
if manifest["requires_octet"] != expected_octet:
    raise SystemExit(
        f"release bundle must declare requires_octet = {expected_octet!r}"
    )
# Native payloads are build outputs, deliberately absent from git ls-files.
# Stage only the explicitly targeted runner and manifest; never ship a launcher
# that would compile at the user's first start or reuse an ambient PATH binary.
NATIVE_TARGETS = {
    "x86_64-unknown-linux-gnu": ("elf", "x86_64"),
    "x86_64-apple-darwin": ("macho", "x86_64"),
    "aarch64-apple-darwin": ("macho", "aarch64"),
    "x86_64-pc-windows-gnu": ("pe", "x86_64"),
    "x86_64-pc-windows-msvc": ("pe", "x86_64"),
}
ELF_MACHINES = {62: "x86_64", 183: "aarch64"}
MACHO_CPU_TYPES = {0x01000007: "x86_64", 0x0100000C: "aarch64"}
PE_MACHINES = {0x8664: "x86_64", 0xAA64: "aarch64", 0x014C: "x86"}


def native_payload_identity(path):
    """Container format and machine type read from the payload itself.

    Container magic alone is not enough: an arm64 Mach-O, a 32-bit PE, or an
    object file staged under a different target would otherwise be published as
    that target. Each check requires the minimal executable header, not a prefix.
    """
    with path.open("rb") as handle:
        head = handle.read(64)
        if head.startswith(b"\x7fELF"):
            if len(head) < 64:
                raise SystemExit(f"native codemode executable has a truncated ELF header: {path}")
            if head[4] != 2 or head[5] != 1:
                raise SystemExit(
                    f"native codemode executable is not a 64-bit little-endian ELF payload: {path}"
                )
            if int.from_bytes(head[16:18], "little") not in (2, 3):
                raise SystemExit(f"native codemode executable is not an executable ELF payload: {path}")
            machine = int.from_bytes(head[18:20], "little")
            return "elf", ELF_MACHINES.get(machine, f"e_machine {machine}")
        if head[:4] in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf"):
            byte_order = "little" if head[:4] == b"\xcf\xfa\xed\xfe" else "big"
            if len(head) < 32:
                raise SystemExit(f"native codemode executable has a truncated Mach-O header: {path}")
            if int.from_bytes(head[12:16], byte_order) != 2:
                raise SystemExit(f"native codemode executable is not an executable Mach-O payload: {path}")
            cpu_type = int.from_bytes(head[4:8], byte_order)
            return "macho", MACHO_CPU_TYPES.get(cpu_type, f"cputype {cpu_type:#x}")
        if head[:4] in (b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xce"):
            raise SystemExit(f"native codemode executable is a 32-bit Mach-O payload: {path}")
        if head[:4] in (b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca", b"\xca\xfe\xba\xbf"):
            # A universal binary is not the per-target artifact this release
            # publishes, and would hide which machine actually runs.
            raise SystemExit(f"native codemode executable is a universal Mach-O payload: {path}")
        if head.startswith(b"MZ"):
            if len(head) < 0x40:
                raise SystemExit(f"native codemode executable has a truncated DOS header: {path}")
            handle.seek(int.from_bytes(head[0x3C:0x40], "little"))
            header = handle.read(26)
            if len(header) < 26 or header[:4] != b"PE\0\0":
                raise SystemExit(f"native codemode executable has no PE header: {path}")
            optional_header = header[24:26]
            if optional_header != b"\x0b\x02":
                raise SystemExit(
                    f"native codemode executable is not a PE32+ payload: {path}"
                )
            machine = int.from_bytes(header[4:6], "little")
            return "pe", PE_MACHINES.get(machine, f"machine {machine:#x}")
        raise SystemExit(f"native codemode executable has the wrong target format: {path}")


native_staging = None
native_root = None
if package_id == "octet-codemode":
    if target not in NATIVE_TARGETS:
        raise SystemExit(f"unsupported codemode release target: {target!r}")
    suffix = ".exe" if "-windows-" in target else ""
    native = source / "target" / target / "release" / f"octet-codemode{suffix}"
    if native.is_symlink() or not native.is_file() or native.stat().st_size == 0:
        raise SystemExit(f"missing regular native codemode executable: {native}")
    for parent in native.parents:
        if parent == source:
            break
        if parent.is_symlink():
            raise SystemExit(f"native codemode executable has a linked ancestor: {native}")
    expected_container, expected_machine = NATIVE_TARGETS[target]
    container, machine = native_payload_identity(native)
    if (container, machine) != (expected_container, expected_machine):
        raise SystemExit(
            f"native codemode executable does not match {target}: found {machine} {container}, "
            f"expected {expected_machine} {expected_container}: {native}"
        )
    native_staging = tempfile.TemporaryDirectory(prefix="octet-codemode-payload-")
    native_root = pathlib.Path(native_staging.name)
    (native_root / "bin").mkdir()
    shutil.copyfile(native, native_root / "bin" / f"codemode{suffix}")
    (native_root / "bin" / f"codemode{suffix}").chmod(0o755)
    text = (source / "extension.toml").read_text(encoding="utf-8")
    old = 'command = "bin/codemode"'
    if text.count(old) != 1:
        raise SystemExit("unexpected codemode entrypoint contract")
    text = text.replace(old, f'command = "bin/codemode{suffix}"')
    (native_root / "extension.toml").write_text(text, encoding="utf-8", newline="\n")
    manifest = tomllib.loads(text)

entrypoint = manifest["entrypoint"]
if not isinstance(entrypoint, dict) or not isinstance(entrypoint.get("command"), str):
    raise SystemExit("extension.toml entrypoint.command must be a string")
command = pathlib.PurePosixPath(entrypoint["command"])
if not command.is_absolute() and (not command.parts or ".." in command.parts or "." in command.parts):
    raise SystemExit("relative entrypoint.command is not portable")
local_entrypoint = None

def entrypoint_is_executable(path, mode, platform):
    if platform != "nt":
        return bool(mode & 0o111)
    # Windows has no POSIX executable bits. Match the native host's bounded
    # Python-script detection, or admit an explicitly named native PE payload.
    # A POSIX shell launcher is not a Windows executable, even after chmod.
    if path.suffix.casefold() == ".py":
        return True
    with path.open("rb") as handle:
        first_line = handle.read(256).split(b"\n", 1)[0]
    if path.suffix.casefold() == ".exe":
        return first_line.startswith(b"MZ")
    return first_line.startswith(b"#!") and b"python" in first_line

if not command.is_absolute():
    local_command = (native_root or source).joinpath(*command.parts)
    if local_command.exists():
        local_entrypoint = command
        command_metadata = local_command.lstat()
        if not stat.S_ISREG(command_metadata.st_mode) or not entrypoint_is_executable(
            local_command, command_metadata.st_mode, os.name
        ):
            raise SystemExit("local entrypoint.command must be a regular executable file")
    elif len(command.parts) > 1:
        raise SystemExit("relative entrypoint.command is missing from the bundle")

if tracked_manifest:
    raw = pathlib.Path(tracked_manifest).read_bytes().split(b"\0")
    files = []
    prefix = pathlib.PurePosixPath("extensions") / package_id
    for encoded in raw:
        if not encoded:
            continue
        try:
            relative_to_repo = pathlib.PurePosixPath(encoded.decode("utf-8"))
        except UnicodeDecodeError as error:
            raise SystemExit("tracked extension path is not UTF-8") from error
        try:
            relative = relative_to_repo.relative_to(prefix)
        except ValueError as error:
            raise SystemExit(f"tracked path escaped extension root: {relative_to_repo}") from error
        files.append(source.joinpath(*relative.parts))
else:
    # is_dir follows links: retain linked directories so the regular-file
    # admission below rejects them instead of silently omitting their contents.
    files = [path for path in source.rglob("*") if path.is_symlink() or not path.is_dir()]

files = [
    path
    for path in files
    if "__pycache__" not in path.relative_to(source).parts
    and path.suffix not in {".pyc", ".pyo"}
    and not (native_root and path.relative_to(source).parts[0] == "target")
    and not (dependency_directory and any(
        part in {"node_modules", ".npm-cache"} for part in path.relative_to(source).parts
    ))
]

if not files:
    raise SystemExit("extension source has no files")
file_roots = {path: source for path in files}
if native_root:
    file_roots.pop(source / "extension.toml", None)
    file_roots.pop(source / "bin/codemode", None)
    for path in native_root.rglob("*"):
        if path.is_file():
            file_roots[path] = native_root
if dependency_directory:
    dependency_root = pathlib.Path(dependency_directory)
    for name in ("package.json", "package-lock.json", "bridge.json", "LICENSE.pi"):
        path = source / name
        if path not in file_roots or not stat.S_ISREG(path.lstat().st_mode):
            raise SystemExit(f"Pi adapter release requires a regular {name}")
    bridge = json.loads((source / "bridge.json").read_text(encoding="utf-8"))
    if bridge != {"extensions": [], "registrations": {key: [] for key in (
        "tools", "commands", "hooks", "flags", "shortcuts", "events", "tool_renderers"
    )}}:
        raise SystemExit("Pi adapter release must ship empty reviewed-factory configuration")
    for name in ("package.json", "package-lock.json"):
        shutil.copyfile(source / name, dependency_root / name)
    npm = shutil.which("npm")
    if npm is None:
        raise SystemExit("required release command is unavailable: npm")
    subprocess.run([
        npm, "ci", "--omit=dev", "--ignore-scripts", "--no-audit", "--no-fund",
        "--bin-links=false",
    ], cwd=dependency_root, check=True)
    modules = dependency_root / "node_modules"
    for path in modules.rglob("*"):
        if path.is_symlink():
            raise SystemExit(f"release bundle cannot contain links or special files: {path}")
        if not path.is_dir():
            file_roots[path] = dependency_root
    # Pi's published TUI tarball omits its MIT license, as codemode's does.
    # Retain the reviewed upstream notice alongside the bundled package.
    license_path = modules / "@earendil-works/pi-tui/LICENSE"
    shutil.copyfile(source / "LICENSE.pi", license_path)
    file_roots[license_path] = dependency_root

relative_files = []
for path, root in file_roots.items():
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode):
        raise SystemExit(f"release bundle cannot contain links or special files: {path}")
    try:
        relative = path.relative_to(root)
    except ValueError as error:
        raise SystemExit(f"extension file escaped source root: {path}") from error
    if not relative.parts or any(part in ("", ".", "..") for part in relative.parts):
        raise SystemExit(f"extension path is not portable: {relative}")
    for parent in path.parents:
        if parent == root:
            break
        if parent.is_symlink():
            raise SystemExit(f"release bundle cannot contain linked ancestors: {path}")
    relative_text = relative.as_posix()
    relative_text.encode("utf-8")
    if any(ord(character) < 32 or ord(character) == 127 for character in relative_text):
        raise SystemExit(f"extension path contains a control character: {relative!r}")
    relative_files.append((relative, path, metadata))

relative_files.sort(key=lambda item: item[0].as_posix())
if [relative.as_posix() for relative, _, _ in relative_files].count("extension.toml") != 1:
    raise SystemExit("release bundle must contain exactly one extension.toml")

if local_entrypoint is not None and local_entrypoint.as_posix() not in {
    relative.as_posix() for relative, _, _ in relative_files
}:
    raise SystemExit("local entrypoint.command is missing from the bundle")

directories = {pathlib.PurePosixPath()}
for relative, _, _ in relative_files:
    parent = pathlib.PurePosixPath(relative.as_posix()).parent
    while parent != pathlib.PurePosixPath("."):
        directories.add(parent)
        parent = parent.parent

with archive.open("wb") as raw:
    with gzip.GzipFile(filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=epoch) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as output:
            root_info = tarfile.TarInfo(package_id)
            root_info.type = tarfile.DIRTYPE
            root_info.mode = 0o755
            root_info.mtime = epoch
            root_info.uid = root_info.gid = 0
            root_info.uname = root_info.gname = ""
            output.addfile(root_info)

            for relative in sorted(directories, key=lambda path: path.as_posix()):
                if not relative.parts:
                    continue
                info = tarfile.TarInfo(f"{package_id}/{relative.as_posix()}")
                info.type = tarfile.DIRTYPE
                info.mode = 0o755
                info.mtime = epoch
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                output.addfile(info)

            for relative, path, metadata in relative_files:
                info = tarfile.TarInfo(f"{package_id}/{relative.as_posix()}")
                info.size = metadata.st_size
                is_entrypoint = local_entrypoint is not None and relative.as_posix() == local_entrypoint.as_posix()
                info.mode = 0o755 if metadata.st_mode & 0o111 or is_entrypoint else 0o644
                info.mtime = epoch
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                with path.open("rb") as contents:
                    output.addfile(info, contents)

with tarfile.open(archive, mode="r:gz") as packaged:
    members = packaged.getmembers()
    if not members or members[0].name.rstrip("/") != package_id or not members[0].isdir():
        raise SystemExit("release bundle has no canonical root directory")
    if any(not (member.isdir() or member.isfile()) for member in members):
        raise SystemExit("release bundle contains a link or special entry")
    names = [member.name.rstrip("/") for member in members]
    if names.count(f"{package_id}/extension.toml") != 1:
        raise SystemExit("release bundle has no unique extension.toml")
PY
printf 'created %s\n' "$archive"
