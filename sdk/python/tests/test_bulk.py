from dataclasses import dataclass
import hashlib
import io
import json
from pathlib import Path
import tempfile
from typing import Optional
import unittest
from unittest.mock import PropertyMock, patch

from octet_extension import BlobDigest, BlobRef, CancelledError, CancellationToken, Extension, RpcError
from octet_extension.bulk import _secure_local_file_available
from octet_extension.typed import _root_codec


LIMITS = {"object_bytes": 1024 * 1024, "owner_bytes": 2 * 1024 * 1024,
          "write_tickets_per_generation": 8, "read_leases_per_generation": 32, "blobs_per_owner": 256}


SECURE_TRANSPORT = unittest.skipUnless(
    _secure_local_file_available(), "local-file.v1 needs dir_fd, O_NOFOLLOW and O_DIRECTORY")


class BulkPlatformTests(unittest.TestCase):
    def test_missing_secure_file_primitives_fail_closed(self):
        bulk = Extension(api_version="0.4", stderr=io.StringIO()).enable_bulk()
        with tempfile.TemporaryDirectory() as root, \
                patch("octet_extension.bulk._secure_local_file_available", return_value=False), \
                self.assertRaises(RpcError) as raised:
            bulk._configure({"profile": "local-file.v1", "transfer_directory": root, "limits": LIMITS})
        self.assertEqual(raised.exception.message, "secure local-file.v1 transport is unavailable")
        with self.assertRaises(RpcError):
            bulk.publish_bytes(b"a")


@SECURE_TRANSPORT
class BulkTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.ext = Extension(api_version="0.4", stderr=io.StringIO())
        self.bulk = self.ext.enable_bulk()
        self.ext._initialize({"api_version": "0.4", "protocol": {
            "version": "0.4", "required_features": ["request_cancellation", "content_parts"],
            "optional_features": ["bulk_objects_v1"], "limits": {"max_concurrent_requests": 1},
            "bulk_objects_v1": {"profile": "local-file.v1", "transfer_directory": str(self.root), "limits": LIMITS}}})
        self.addCleanup(self.ext._executor.shutdown)
        self.calls, self.data = [], b""
        self.ext.request = self.request

    def reference(self, data=None):
        data = self.data if data is None else data
        return BlobRef("blob-id", len(data), BlobDigest("sha256", hashlib.sha256(data).hexdigest()), "application/octet-stream")

    def request(self, method, params, **kwargs):
        self.assertEqual(kwargs, {"operation_scoped": True})
        json.dumps(params)  # No bytes ever enter JSON-RPC.
        self.calls.append((method, params))
        if method == "bulk/write":
            (self.root / "opaque-write-file").write_bytes(b"")
            return {"ticket": "write-id", "profile": "local-file.v1", "locator": "opaque-write-file", "capacity": params["capacity"]}
        if method == "bulk/commit":
            self.data = (self.root / "opaque-write-file").read_bytes()
            self.assertEqual(params["digest"], self.reference().digest.to_wire())
            return self.reference().to_wire()
        if method == "bulk/read":
            (self.root / "opaque-read-file").write_bytes(self.data)
            return {"lease": "read-id", "profile": "local-file.v1", "locator": "opaque-read-file", "bytes": len(self.data)}
        if method == "bulk/release":
            return {"released": True}
        self.fail(f"unexpected request: {method}")

    def test_closed_codec_and_automatic_feature_opt_in(self):
        ext = Extension(api_version="0.4")
        @dataclass
        class Input:
            prior: Optional[BlobRef] = None
        @dataclass
        class Output:
            blobs: list[BlobRef]
        @ext.typed_tool(name="blob", description="Typed bulk", summary=lambda value: "Stored bytes.")
        def handler(args: Input) -> Output:
            return Output([])
        self.assertIn("bulk_objects_v1", ext._supported_features)
        self.assertNotIn("operation_descriptors_v1", ext._supported_features)
        schema = ext._tools["blob"].output_schema["properties"]["blobs"]["items"]
        self.assertEqual(set(schema["properties"]), {"$blob", "bytes", "digest", "media_type"})
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(schema["properties"]["digest"]["properties"]["algorithm"]["enum"], ["sha256"])
        codec = _root_codec(Output)
        value = Output([self.reference(b"x")])
        self.assertEqual(codec.decode(codec.encode(value)), value)
        for invalid in [self.reference().to_wire() | {"locator": "private"},
                        {**self.reference().to_wire(), "digest": {"algorithm": "md5", "value": "0" * 64}},
                        {**self.reference().to_wire(), "bytes": True}]:
            with self.assertRaises(ValueError):
                BlobRef.from_wire(invalid)

    def test_blob_schema_supported_bounds_and_strict_metadata_codecs(self):
        codec = _root_codec(BlobRef)
        properties = codec.schema["properties"]
        self.assertEqual(properties["$blob"], {"type": "string", "minLength": 1, "maxLength": 128})
        self.assertEqual(properties["digest"]["properties"]["value"],
                         {"type": "string", "minLength": 64, "maxLength": 64})
        wire = {**self.reference().to_wire(), "$blob": "b" * 128}
        self.assertEqual(codec.encode(codec.decode(wire)), wire)
        for identity in ["", "b" * 129, "é", "line\nbreak"]:
            with self.subTest(identity=identity):
                with self.assertRaises(ValueError):
                    codec.decode({**wire, "$blob": identity})
                native = self.reference()
                object.__setattr__(native, "id", identity)
                with self.assertRaises(ValueError):
                    codec.encode(native)
        for digest in ["a" * 63, "a" * 65, "A" * 64, "g" * 64]:
            with self.subTest(digest=digest):
                with self.assertRaises(ValueError):
                    codec.decode({**wire, "digest": {"algorithm": "sha256", "value": digest}})
                native = self.reference()
                object.__setattr__(native.digest, "value", digest)
                with self.assertRaises(ValueError):
                    codec.encode(native)

    def test_publish_read_and_exact_empty_binary_roundtrip(self):
        for data in [b"", bytes(range(256)) * 400]:
            reference = self.bulk.publish_bytes(data)
            self.assertEqual(reference, self.reference(data))
            with self.bulk.read(reference, max_bytes=len(data)) as stream:
                self.assertEqual(stream.read(), data)
                self.assertFalse(hasattr(stream, "name"))
            self.assertTrue(stream.closed)
            self.assertEqual(self.calls[-1], ("bulk/release", {"id": "read-id"}))
            self.assertNotIn("opaque", json.dumps(reference.to_wire()))

    def test_read_bound_and_integrity_failure_never_yield_unverified_data(self):
        self.data = b"bad"
        reference = self.reference(b"abc")
        with self.assertRaises(ValueError), self.bulk.read(reference, max_bytes=3):
            self.fail("unverified bytes were exposed")
        self.assertEqual(self.calls[-1][0], "bulk/release")
        count = len(self.calls)
        with self.assertRaises(ValueError), self.bulk.read(reference, max_bytes=2):
            self.fail("over-bound bytes were exposed")
        with self.assertRaises(ValueError), self.bulk.read(reference, max_bytes=LIMITS["object_bytes"] + 1):
            self.fail("over-bound read was allowed")
        self.assertEqual(len(self.calls), count)

    def test_parser_failure_and_cancellation_close_reader_and_release(self):
        reference = self.bulk.publish_bytes(b"abc")
        for exception in [ValueError("parser failed"), CancelledError()]:
            with self.assertRaises(type(exception)):
                with self.bulk.read(reference, max_bytes=3) as stream:
                    raise exception
            self.assertTrue(stream.closed)
            self.assertEqual(self.calls[-1], ("bulk/release", {"id": "read-id"}))

    def test_cancelled_writer_abandons_ticket_without_committing(self):
        token = CancellationToken(1)
        def request(method, params, **kwargs):
            result = self.request(method, params, **kwargs)
            if method == "bulk/write":
                token._cancel("test")
            return result
        self.ext.request = request
        with patch.object(Extension, "cancellation", new_callable=PropertyMock, return_value=token):
            with self.assertRaises(CancelledError):
                self.bulk.publish_bytes(b"abc")
        self.assertEqual([name for name, _ in self.calls], ["bulk/write", "bulk/release"])

    def test_traversal_and_symlink_grants_are_refused_and_abandoned(self):
        outside = self.root / "outside"
        outside.write_bytes(b"unchanged")
        for locator in ["../outside", str(outside), "link"]:
            link = self.root / "link"
            if not link.exists():
                link.symlink_to(outside)
            def request(method, params, **kwargs):
                result = self.request(method, params, **kwargs)
                if method == "bulk/write":
                    result["locator"] = locator
                return result
            self.ext.request = request
            with self.assertRaises((ValueError, RpcError)):
                self.bulk.publish_bytes(b"altered")
            self.assertEqual(outside.read_bytes(), b"unchanged")
            self.assertEqual(self.calls[-1], ("bulk/release", {"id": "write-id"}))

    def test_unsupported_profile_missing_feature_and_size_limits_fail_closed(self):
        with self.assertRaises(RpcError):
            self.bulk._configure({"profile": "inline-json", "transfer_directory": str(self.root), "limits": LIMITS})
        self.ext._features = frozenset()
        with self.assertRaises(RpcError):
            self.bulk.publish_bytes(b"a")
        self.ext._features = frozenset({"bulk_objects_v1"})
        with self.assertRaises(ValueError):
            self.bulk.publish_bytes(b"a" * (LIMITS["object_bytes"] + 1))
        for invalid in ["*/*", "text/plain;", "text/plain; x=", "text/plain\nsecret", "text/plain; x=\"open"]:
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                self.bulk.publish_bytes(b"a", media_type=invalid)
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
