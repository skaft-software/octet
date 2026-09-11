"""POSIX owner-private plaintext token storage; not an encrypted vault.

No environment, project path, legacy credential file, or host secret broker is
consulted. The production host has no writable secret broker. Callers supply a
host-selected absolute state directory (normally ~/.octet/mcp-auth). Every path
component is opened no-follow. Private directories are 0700, files 0600, regular,
current-uid and single-link. Locks/atomic rename/fsync serialize refresh rotation
across processes without ever reusing a refresh token after an ambiguous exchange.

This protects against other OS users, not the same OS principal, root, backups,
malicious extensions, or a compromised host. Unlink is not secure erasure.
"""

from __future__ import annotations

from contextlib import contextmanager
from dataclasses import asdict
import fcntl
import json
import math
import os
from pathlib import Path
import secrets
import stat
import time
from typing import Iterator, Optional

from .auth import (AuthBinding, AuthError, Cancel, MAX_DOCUMENT_BYTES, TokenRecord,
                   check_operation, decode_document, secret_text)


def default_store_path() -> Path:
    return Path.home() / ".octet" / "mcp-auth"


def _validate_file(info: os.stat_result) -> None:
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
            or stat.S_IMODE(info.st_mode) != 0o600 or info.st_nlink != 1
            or info.st_size > MAX_DOCUMENT_BYTES):
        raise AuthError("authentication_storage")


class PrivateTokenStore:
    """Inert construction. Disk is touched only by an explicit scoped operation."""

    def __init__(self, root: Path) -> None:
        self._root = os.fspath(root)

    def _directory(self) -> int:
        parts = self._root.split("/")
        if (not self._root.startswith("/") or len(parts) > 65
                or any(part in {"", ".", ".."} for part in parts[1:])
                or not all(hasattr(os, flag) for flag in ("O_NOFOLLOW", "O_DIRECTORY"))):
            raise AuthError("authentication_storage")
        flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
        descriptor = os.open("/", flags)
        try:
            for index, part in enumerate(parts[1:]):
                final = index == len(parts) - 2
                try:
                    child = os.open(part, flags, dir_fd=descriptor)
                except FileNotFoundError:
                    # Create only under an already validated ancestor, with a
                    # no-follow reopen so mkdir races cannot bless a link.
                    try:
                        os.mkdir(part, 0o700, dir_fd=descriptor)
                    except FileExistsError:
                        pass
                    child = os.open(part, flags, dir_fd=descriptor)
                os.close(descriptor)
                descriptor = child
                info = os.fstat(descriptor)
                mode = stat.S_IMODE(info.st_mode)
                if final:
                    valid = info.st_uid == os.getuid() and mode == 0o700
                else:
                    # Root-owned sticky system temp ancestors are acceptable;
                    # their private child still has to pass the checks above.
                    valid = info.st_uid in {0, os.getuid()} and (
                        not mode & 0o022 or (info.st_uid == 0 and bool(mode & stat.S_ISVTX))
                    )
                if not valid:
                    raise AuthError("authentication_storage")
            return descriptor
        except BaseException:
            os.close(descriptor)
            raise

    @contextmanager
    def transaction(self, binding: AuthBinding, *, deadline: float,
                    cancel: Cancel) -> Iterator["TokenTransaction"]:
        directory = lock = None
        try:
            check_operation(deadline, cancel)
            directory = self._directory()
            lock = os.open(binding.key + ".lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW
                           | os.O_CLOEXEC | os.O_NONBLOCK, 0o600, dir_fd=directory)
            _validate_file(os.fstat(lock))
            # Contention is not invalid authentication. Wait within the caller's
            # budget so it can reread the winner's persisted replacement, while
            # polling cancellation/owner revocation instead of blocking in flock.
            while True:
                check_operation(deadline, cancel)
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    time.sleep(min(0.05, max(0.0, deadline - time.monotonic())))
            check_operation(deadline, cancel)
            yield TokenTransaction(directory, binding, deadline, cancel)
        except OSError:
            raise AuthError("authentication_storage") from None
        finally:
            if lock is not None:
                os.close(lock)
            if directory is not None:
                os.close(directory)


class TokenTransaction:
    def __init__(self, directory: int, binding: AuthBinding, deadline: float,
                 cancel: Cancel) -> None:
        self._directory = directory
        self._binding = binding
        self._name = binding.key + ".json"
        self._deadline = deadline
        self._cancel = cancel

    def _read(self) -> Optional[bytes]:
        try:
            descriptor = os.open(self._name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
                                 | os.O_CLOEXEC, dir_fd=self._directory)
        except FileNotFoundError:
            return None
        try:
            _validate_file(os.fstat(descriptor))
            with os.fdopen(descriptor, "rb", closefd=False) as stream:
                data = stream.read(MAX_DOCUMENT_BYTES + 1)
            if len(data) > MAX_DOCUMENT_BYTES:
                raise AuthError("authentication_storage")
            return data
        finally:
            os.close(descriptor)

    def load(self) -> Optional[TokenRecord]:
        check_operation(self._deadline, self._cancel)
        data = self._read()
        if data is None:
            return None
        try:
            value = decode_document(data)
            if (set(value) != {"version", "binding", "token"} or value["version"] != 1
                    or value["binding"] != self._binding.key):
                raise ValueError()
            token = value["token"]
            if not isinstance(token, dict) or set(token) != {
                "access_token", "refresh_token", "expires_at", "token_endpoint", "scopes"
            }:
                raise ValueError()
            secret_text(token["access_token"], bearer=True)
            if token["refresh_token"] is not None:
                secret_text(token["refresh_token"])
            expiry = token["expires_at"]
            if expiry is not None and (type(expiry) not in (int, float)
                                       or not math.isfinite(expiry) or expiry <= 0):
                raise ValueError()
            if self._binding.kind == "oauth":
                from .oauth import https_url, scope_list
                https_url(token["token_endpoint"])
                scope_list(token["scopes"])
                if expiry is None:
                    raise ValueError()
            elif (token["refresh_token"] is not None or token["token_endpoint"] is not None
                  or token["scopes"] != [] or expiry is not None):
                raise ValueError()
            token["scopes"] = tuple(token["scopes"])
            return TokenRecord(**token)
        except (ValueError, TypeError, KeyError):
            raise AuthError("authentication_storage") from None

    def save(self, token: TokenRecord) -> None:
        check_operation(self._deadline, self._cancel)
        data = json.dumps({"version": 1, "binding": self._binding.key,
                           "token": asdict(token)}, separators=(",", ":")).encode("utf-8")
        if len(data) > MAX_DOCUMENT_BYTES:
            raise AuthError("authentication_storage")
        self._read()  # Reject unsafe existing files rather than replacing them.
        temporary = "." + secrets.token_hex(24) + ".tmp"
        descriptor = None
        try:
            descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL
                                 | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600,
                                 dir_fd=self._directory)
            _validate_file(os.fstat(descriptor))
            with os.fdopen(descriptor, "wb", closefd=False) as stream:
                stream.write(data)
                stream.flush()
                os.fsync(descriptor)
            check_operation(self._deadline, self._cancel)
            os.replace(temporary, self._name, src_dir_fd=self._directory,
                       dst_dir_fd=self._directory)
            os.fsync(self._directory)
        finally:
            if descriptor is not None:
                os.close(descriptor)
            try:
                os.unlink(temporary, dir_fd=self._directory)
            except FileNotFoundError:
                pass

    def delete(self) -> bool:
        check_operation(self._deadline, self._cancel)
        if self._read() is None:
            return False
        os.unlink(self._name, dir_fd=self._directory)
        os.fsync(self._directory)
        return True
