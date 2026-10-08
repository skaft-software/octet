"""Immutable bulk descriptors and parent-scoped local-file.v1 transfers.

Transport locators stay private to this module. Descriptors are metadata, never
read authority; the host authenticates every read/publication and owns retirement.
"""
from contextlib import contextmanager
from dataclasses import dataclass
import hashlib
import io
import os
from pathlib import Path
import re
import stat

from .protocol import RpcError

_PORTABLE = 2**53 - 1
_PROFILE = "local-file.v1"
_LIMIT_KEYS = {"object_bytes", "owner_bytes", "write_tickets_per_generation", "read_leases_per_generation", "blobs_per_owner"}
_TOKEN = re.compile(r"[!#$%&'*+\-.^_`|~0-9A-Za-z]+")
_MEDIA_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9!#$&^_.+\-]{0,126}")


def _identifier(value):
    if type(value) is not str or not 1 <= len(value) <= 128 or any(ord(c) < 33 or ord(c) > 126 for c in value):
        raise ValueError("invalid opaque bulk identity")
    return value


def _size(value, *, maximum=_PORTABLE):
    if type(value) is not int or not 0 <= value <= maximum:
        raise ValueError("bulk size exceeds its finite bound")
    return value


def _media(value):
    if type(value) is not str or not value.isascii() or len(value) > 255 or any(ord(c) < 32 and c != "\t" or ord(c) == 127 for c in value):
        raise ValueError("invalid bulk media type")
    essence, separator, rest = value.partition(";")
    names = essence.rstrip(" \t").split("/")
    if len(names) != 2 or not all(_MEDIA_NAME.fullmatch(name) for name in names) or separator and not rest:
        raise ValueError("invalid bulk media type")
    while rest:
        rest = rest.lstrip(" \t")
        name = _TOKEN.match(rest)
        if name is None or rest[name.end():name.end()+1] != "=":
            raise ValueError("invalid bulk media parameter")
        rest = rest[name.end()+1:]
        if rest.startswith('"'):
            match = re.match(r'"(?:[^"\\]|\\.)*"', rest)
        else:
            match = _TOKEN.match(rest)
        if match is None:
            raise ValueError("invalid bulk media parameter")
        rest = rest[match.end():].lstrip(" \t")
        if rest:
            if not rest.startswith(";") or not rest[1:]:
                raise ValueError("invalid bulk media parameter")
            rest = rest[1:]
    return value


@dataclass(frozen=True)
class BlobDigest:
    algorithm: str
    value: str

    def __post_init__(self):
        if self.algorithm != "sha256" or type(self.value) is not str or re.fullmatch(r"[0-9a-f]{64}", self.value) is None:
            raise ValueError("bulk digest must be lowercase SHA-256")

    def to_wire(self):
        return {"algorithm": self.algorithm, "value": self.value}


@dataclass(frozen=True)
class BlobRef:
    """Closed metadata for host-verified immutable bytes, not a transport locator."""
    id: str
    bytes: int
    digest: BlobDigest
    media_type: str

    def __post_init__(self):
        _identifier(self.id)
        _size(self.bytes)
        _media(self.media_type)
        if type(self.digest) is not BlobDigest:
            raise ValueError("BlobRef requires BlobDigest")

    def to_wire(self):
        return {"$blob": self.id, "bytes": self.bytes, "digest": self.digest.to_wire(), "media_type": self.media_type}

    @classmethod
    def from_wire(cls, value):
        if type(value) is not dict or set(value) != {"$blob", "bytes", "digest", "media_type"}:
            raise ValueError("invalid closed BlobRef")
        digest = value["digest"]
        if type(digest) is not dict or set(digest) != {"algorithm", "value"}:
            raise ValueError("invalid closed BlobDigest")
        return cls(value["$blob"], value["bytes"], BlobDigest(**digest), value["media_type"])


def _blob_codec():
    from .typed import _Codec
    schema = {"type": "object", "additionalProperties": False,
              "required": ["$blob", "bytes", "digest", "media_type"], "properties": {
                  "$blob": {"type": "string", "minLength": 1, "maxLength": 128},
                  "bytes": {"type": "integer", "minimum": 0, "maximum": _PORTABLE},
                  "media_type": {"type": "string", "minLength": 1, "maxLength": 255},
                  "digest": {"type": "object", "additionalProperties": False, "required": ["algorithm", "value"],
                             "properties": {"algorithm": {"type": "string", "enum": ["sha256"]},
                                            "value": {"type": "string", "minLength": 64, "maxLength": 64}}}}}
    def convert(value, encode):
        if encode:
            if type(value) is not BlobRef:
                raise ValueError("typed blob output requires BlobRef")
            return BlobRef.from_wire(value.to_wire()).to_wire()
        return BlobRef.from_wire(value)
    return _Codec(schema, convert, features=frozenset({"bulk_objects_v1"}))


def _secure_local_file_available():
    """Whether this platform has the primitives local-file.v1 requires (not Windows)."""
    return os.open in os.supports_dir_fd and hasattr(os, "O_NOFOLLOW") and hasattr(os, "O_DIRECTORY")


class Bulk:
    """Bounded bytes publication/read helpers on the existing RPC execution lane."""
    def __init__(self, extension):
        self._extension = extension
        self._directory = None
        self._limits = None

    def _configure(self, context):
        if type(context) is not dict or set(context) != {"profile", "transfer_directory", "limits"} or context["profile"] != _PROFILE:
            raise RpcError(-32000, "unsupported bulk transport profile")
        root, limits = context["transfer_directory"], context["limits"]
        if type(root) is not str or not Path(root).is_absolute() or "\x00" in root or len(root.encode("utf-8")) > 4096:
            raise RpcError(-32000, "invalid bulk transport context")
        if type(limits) is not dict or set(limits) != _LIMIT_KEYS or any(type(v) is not int or not 0 < v <= _PORTABLE for v in limits.values()):
            raise RpcError(-32000, "invalid finite bulk limits")
        if not _secure_local_file_available():
            raise RpcError(-32000, "secure local-file.v1 transport is unavailable")
        self._directory, self._limits = root, dict(limits)

    def _check(self):
        self._extension._require_feature("bulk_objects_v1")
        if self._directory is None:
            raise RpcError(-32000, "bulk transport is unavailable")
        cancellation = self._extension.cancellation
        if cancellation is not None:
            cancellation.raise_if_cancelled()

    @contextmanager
    def _file(self, locator, *, write):
        # A host-issued basename, never a result field or an author-supplied path.
        if type(locator) is not str or not 1 <= len(locator) <= 256 or locator in (".", "..") or any(c in locator for c in ("/", "\\", "\x00")):
            raise ValueError("invalid bulk transport locator")
        descriptor = None
        try:
            root = os.open(self._directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                descriptor = os.open(locator, (os.O_WRONLY if write else os.O_RDONLY) | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=root)
            finally:
                os.close(root)
            if not stat.S_ISREG(os.fstat(descriptor).st_mode):
                raise ValueError("bulk transfer must be a regular file")
            stream = os.fdopen(descriptor, "wb" if write else "rb")
            descriptor = None
        except OSError:
            raise RpcError(-32000, "bulk transfer file unavailable") from None
        finally:
            if descriptor is not None:
                os.close(descriptor)
        with stream:
            yield stream

    def _release(self, identity, *, suppress):
        try:
            result = self._extension.request("bulk/release", {"id": identity}, operation_scoped=True)
            if type(result) is not dict or result != {"released": True} or result["released"] is not True:
                raise RpcError(-32000, "invalid bulk release acknowledgement")
        except RpcError:
            # Parent cancellation/retirement may already have revoked the grant.
            # The host owns all abandoned tickets/leases, including lost replies.
            if not suppress:
                raise

    def publish_bytes(self, data: bytes, *, media_type: str = "application/octet-stream") -> BlobRef:
        """Commit owned bytes provisionally; only a valid success publishes them."""
        self._check()
        if type(data) is not bytes:
            raise TypeError("publish_bytes requires immutable bytes")
        _size(len(data), maximum=self._limits["object_bytes"])
        _media(media_type)
        ticket = self._extension.request("bulk/write", {"profile": _PROFILE, "capacity": len(data), "media_type": media_type}, operation_scoped=True)
        if type(ticket) is not dict or set(ticket) != {"ticket", "profile", "locator", "capacity"} or ticket["profile"] != _PROFILE:
            raise ValueError("invalid bulk write grant")
        identity = _identifier(ticket["ticket"])
        committed = False
        try:
            if type(ticket["capacity"]) is not int or ticket["capacity"] != len(data):
                raise ValueError("bulk write capacity mismatch")
            digest = hashlib.sha256()
            with self._file(ticket["locator"], write=True) as stream:
                for offset in range(0, len(data), 64 * 1024):
                    self._check()
                    chunk = memoryview(data)[offset:offset + 64 * 1024]
                    if stream.write(chunk) != len(chunk):
                        raise RpcError(-32000, "bulk write was incomplete")
                    digest.update(chunk)
                stream.truncate(len(data))
            self._check()
            integrity = BlobDigest("sha256", digest.hexdigest())
            value = self._extension.request("bulk/commit", {"ticket": identity, "bytes": len(data), "digest": integrity.to_wire()}, operation_scoped=True)
            reference = BlobRef.from_wire(value)
            if reference.bytes != len(data) or reference.digest != integrity or reference.media_type != media_type:
                raise ValueError("bulk committed descriptor mismatch")
            committed = True
            return reference
        finally:
            if not committed:
                self._release(identity, suppress=True)

    @contextmanager
    def read(self, reference: BlobRef, *, max_bytes: int):
        """Yield a verified bounded in-memory binary reader; always release lease.

        max_bytes is required and bounded by the negotiated object limit. This
        convenience API snapshots up to that bound; it is not an unbounded stream.
        """
        self._check()
        if type(reference) is not BlobRef:
            raise TypeError("bulk read requires BlobRef")
        _size(max_bytes, maximum=self._limits["object_bytes"])
        if reference.bytes > max_bytes:
            raise ValueError("blob exceeds caller read bound")
        lease = self._extension.request("bulk/read", {"profile": _PROFILE, "blob": reference.to_wire()}, operation_scoped=True)
        if type(lease) is not dict or set(lease) != {"lease", "profile", "locator", "bytes"} or lease["profile"] != _PROFILE:
            raise ValueError("invalid bulk read grant")
        identity = _identifier(lease["lease"])
        failed = True
        try:
            if type(lease["bytes"]) is not int or lease["bytes"] != reference.bytes:
                raise ValueError("bulk read length mismatch")
            with self._file(lease["locator"], write=False) as stream:
                if os.fstat(stream.fileno()).st_size != reference.bytes:
                    raise ValueError("bulk read length mismatch")
                with io.BytesIO() as snapshot:
                    digest, remaining = hashlib.sha256(), reference.bytes
                    while remaining:
                        self._check()
                        chunk = stream.read(min(remaining, 64 * 1024))
                        if not chunk:
                            raise ValueError("bulk read was incomplete")
                        remaining -= len(chunk)
                        digest.update(chunk)
                        snapshot.write(chunk)
                    if stream.read(1) or digest.hexdigest() != reference.digest.value:
                        raise ValueError("bulk read integrity mismatch")
                    stream.close()
                    self._check()
                    snapshot.seek(0)
                    yield snapshot
                    failed = False
        finally:
            self._release(identity, suppress=failed)
