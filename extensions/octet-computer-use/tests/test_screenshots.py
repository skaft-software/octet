"""Deterministic contract tests for bounded automation screenshot capture.

These tests use byte fixtures and narrow fake transports.  They intentionally do
not invoke a browser, a host artifact resolver, or an image library.
"""

from __future__ import annotations

import hashlib
import json
import struct
import tempfile
import unittest
import zlib
from dataclasses import replace
from pathlib import Path
from typing import Any, Mapping

from octet_computer_use.screenshots import (
    ArtifactTransportError,
    FreshObservationRequired,
    HostArtifactTransport,
    RequestBudget,
    ResourceOwner,
    ScreenshotBinding,
    ScreenshotError,
    ScreenshotLimits,
    ScreenshotStore,
    TargetIdentity,
)


class ManualClock:
    def __init__(self, value: float = 0.0) -> None:
        self.value = value

    def __call__(self) -> float:
        return self.value


class RecordingTransport:
    def __init__(self, artifact_prefix: str = "artifact", artifact_id: str | None = None) -> None:
        self.artifact_prefix = artifact_prefix
        self.fixed_artifact_id = artifact_id
        self.calls: list[dict[str, Any]] = []
        self.assets: dict[str, bytes] = {}

    def publish(
        self,
        *,
        mime_type: str,
        data: bytes,
        size: int,
        sha256: str,
        parent_request_id: Any = None,
    ) -> str:
        self.calls.append(
            {
                "mime_type": mime_type,
                "data": data,
                "size": size,
                "sha256": sha256,
                "parent_request_id": parent_request_id,
            }
        )
        if len(data) != size or hashlib.sha256(data).hexdigest() != sha256:
            raise AssertionError("transport received an invalid integrity claim")
        artifact_id = self.fixed_artifact_id or f"{self.artifact_prefix}-{len(self.calls)}"
        self.assets[artifact_id] = data
        return artifact_id


class RecordingPublisher:
    def __init__(
        self,
        root: Path | None = None,
        *,
        features: set[str] | None = None,
        artifact_id: str = "host-artifact-1",
        failure: Exception | None = None,
    ) -> None:
        self.root = root
        self.negotiated_features = features if features is not None else {"artifacts"}
        self.artifact_id = artifact_id
        self.failure = failure
        self.calls: list[dict[str, Any]] = []
        self.path_bytes: list[bytes] = []

    def publish_artifact(self, **arguments: Any) -> str:
        self.calls.append(dict(arguments))
        if self.failure is not None:
            raise self.failure
        if "path" in arguments:
            if self.root is None:
                raise AssertionError("a scratch root is required for staged publication")
            relative = arguments["path"]
            if not isinstance(relative, str) or relative.startswith("/") or ".." in Path(relative).parts:
                raise AssertionError("the transport must submit a safe relative path")
            data = (self.root / relative).read_bytes()
            self.path_bytes.append(data)
        else:
            data = arguments["data"]
        if len(data) != arguments["size"] or hashlib.sha256(data).hexdigest() != arguments["sha256"]:
            raise AssertionError("publisher received an invalid integrity claim")
        return self.artifact_id


def _png(width: int = 1, height: int = 1, marker: bytes = b"fixture") -> bytes:
    def chunk(kind: bytes, payload: bytes) -> bytes:
        return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)

    rows = b"".join(b"\x00" + b"\x00\x00\x00\xff" * width for _ in range(height))
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return b"".join(
        (
            b"\x89PNG\r\n\x1a\n",
            chunk(b"IHDR", ihdr),
            chunk(b"tEXt", b"fixture\x00" + marker),
            chunk(b"IDAT", zlib.compress(rows)),
            chunk(b"IEND", b""),
        )
    )


def _jpeg(width: int = 1, height: int = 1) -> bytes:
    payload = bytes([8]) + struct.pack(">HH", height, width) + bytes([3]) + b"\x00" * 9
    return b"\xff\xd8\xff\xc0" + struct.pack(">H", len(payload) + 2) + payload + b"\xff\xd9"


def _gif(width: int = 1, height: int = 1) -> bytes:
    return b"GIF89a" + struct.pack("<HH", width, height) + b"\x80\x00\x00"


def _webp(width: int = 1, height: int = 1) -> bytes:
    payload = b"\x00\x00\x00\x00" + (width - 1).to_bytes(3, "little") + (height - 1).to_bytes(3, "little")
    chunk = b"VP8X" + struct.pack("<I", len(payload)) + payload
    return b"RIFF" + struct.pack("<I", len(chunk) + 4) + b"WEBP" + chunk


FRAME_A = _png(marker=b"fixture-a")
FRAME_B = _png(marker=b"fixture-b")
OWNER = ResourceOwner("session-a", "computer-use-a", 1)
OTHER_OWNER = ResourceOwner("session-b", "computer-use-b", 1)
TARGET = TargetIdentity("window", "window-a", "window-a")
OTHER_TARGET = TargetIdentity("window", "window-b", "window-b")
BINDING = ScreenshotBinding(OWNER, TARGET, 1)
OTHER_OWNER_BINDING = ScreenshotBinding(OTHER_OWNER, TARGET, 1)
OTHER_TARGET_BINDING = ScreenshotBinding(OWNER, OTHER_TARGET, 1)
OTHER_GENERATION_BINDING = ScreenshotBinding(OWNER, TARGET, 2)


def _limits(**overrides: Any) -> ScreenshotLimits:
    values: dict[str, Any] = {"inline_artifact_bytes": len(FRAME_A)}
    values.update(overrides)
    return ScreenshotLimits(**values)


def _assert_no_data_key(value: Any, testcase: unittest.TestCase) -> None:
    if isinstance(value, Mapping):
        testcase.assertNotIn("data", value)
        for child in value.values():
            _assert_no_data_key(child, testcase)
    elif isinstance(value, (list, tuple)):
        for child in value:
            _assert_no_data_key(child, testcase)


class ScreenshotStoreTests(unittest.TestCase):
    def test_selection_capacity_cannot_exceed_retained_frame_capacity(self) -> None:
        with self.assertRaises(ScreenshotError) as invalid:
            _limits(max_frames=3, max_selected_images=4)
        self.assertEqual(invalid.exception.code, "invalid_limits")

    def test_hundred_identical_frames_deduplicate_and_projection_is_reference_only(self) -> None:
        clock = ManualClock()
        transport = RecordingTransport()
        store = ScreenshotStore(
            transport,
            limits=_limits(
                max_frames=101,
                max_frame_encoded_bytes=len(FRAME_A),
                max_retained_bytes=len(FRAME_A),
            ),
            clock=clock,
        )

        results = [store.capture(FRAME_A, binding=BINDING, parent_request_id="request-1") for _ in range(100)]
        self.assertTrue(all(result.captured for result in results))
        self.assertTrue(results[0].published_new_asset)
        self.assertTrue(all(result.deduplicated_asset for result in results[1:]))
        self.assertEqual(len(transport.calls), 1)
        self.assertEqual(store.metrics()["admitted_frame_count"], 100)
        self.assertEqual(store.metrics()["unique_asset_count"], 1)
        self.assertEqual(store.metrics()["admitted_encoded_bytes"], len(FRAME_A))
        self.assertEqual(transport.calls[0]["parent_request_id"], "request-1")

        over_bytes = store.capture(FRAME_B, binding=BINDING)
        self.assertFalse(over_bytes.captured)
        self.assertEqual(over_bytes.omission.reason, "retained_byte_budget")
        self.assertEqual(len(transport.calls), 1)

        for _ in range(20):
            projection = store.project(binding=BINDING, fresh=results[-1], text="follow-up observation")
            request = projection.to_request()
            self.assertEqual(len(request["content"]), 2)
            self.assertEqual(request["content"][0], {"type": "text", "text": "follow-up observation"})
            self.assertEqual(
                set(request["content"][1]),
                {"type", "artifact_id", "mime_type", "alt"},
            )
            self.assertEqual(request["content"][1]["artifact_id"], results[-1].artifact_id)
            self.assertEqual(projection.selected_image_count, 1)
            _assert_no_data_key(request, self)
            _assert_no_data_key(projection.metadata(), self)

        history = store.canonical_history(binding=BINDING)
        self.assertLessEqual(len(history), 128)
        self.assertEqual(len(history), 101)
        _assert_no_data_key(history, self)
        self.assertEqual(json.dumps(history, sort_keys=True).count("fixture-a"), 0)

    def test_owner_target_and_authorization_generation_fences_apply_to_reads_and_history(self) -> None:
        store = ScreenshotStore(RecordingTransport(), clock=ManualClock())
        result = store.capture(FRAME_A, binding=BINDING)
        assert result.reference is not None

        self.assertIsNone(store.get(result.reference, binding=OTHER_OWNER_BINDING))
        self.assertIsNone(store.get(result.reference, binding=OTHER_TARGET_BINDING))
        self.assertIsNone(store.get(result.reference, binding=OTHER_GENERATION_BINDING))
        self.assertEqual(store.history(binding=OTHER_OWNER_BINDING), ())
        self.assertEqual(store.history(owner=OTHER_OWNER), ())

        owner_projection = store.project(binding=OTHER_OWNER_BINDING, fresh=result)
        target_projection = store.project(binding=OTHER_TARGET_BINDING, fresh=result)
        generation_projection = store.project(binding=OTHER_GENERATION_BINDING, fresh=result)
        self.assertEqual(owner_projection.omissions[0].reason, "owner_mismatch")
        self.assertEqual(target_projection.omissions[0].reason, "target_mismatch")
        self.assertEqual(generation_projection.omissions[0].reason, "authorization_generation_mismatch")
        self.assertEqual(owner_projection.image_parts, ())
        self.assertEqual(target_projection.image_parts, ())
        self.assertEqual(generation_projection.image_parts, ())

        context_result = store.capture_from_context(
            FRAME_B,
            {"resource_owner": OWNER.as_dict()},
            target=TARGET.as_dict(),
            authorization_generation=1,
        )
        self.assertTrue(context_result.captured)
        with self.assertRaisesRegex(ScreenshotError, "owner") as error:
            store.capture_from_context(FRAME_A, {}, target=TARGET, authorization_generation=1)
        self.assertEqual(error.exception.code, "owner_unavailable")

    def test_digest_metadata_and_host_publication_integrity_are_checked(self) -> None:
        transport = RecordingTransport()
        store = ScreenshotStore(transport, clock=ManualClock())
        result = store.capture(FRAME_A, binding=BINDING)
        assert result.reference is not None
        reference = result.reference
        self.assertEqual(reference.sha256, hashlib.sha256(FRAME_A).hexdigest())
        self.assertEqual(reference.content_id, "sha256:" + reference.sha256)
        self.assertEqual(reference.encoded_bytes, len(FRAME_A))
        self.assertFalse(reference.action_eligible)
        self.assertEqual(transport.calls[0]["sha256"], reference.sha256)

        with self.assertRaises(ScreenshotError) as mismatch:
            store.capture(FRAME_A, binding=BINDING, width=2, height=1)
        self.assertEqual(mismatch.exception.code, "screenshot_metadata_mismatch")
        self.assertEqual(len(transport.calls), 1)

        with self.assertRaises(ScreenshotError) as invalid_id:
            ScreenshotStore(
                RecordingTransport(artifact_id="not a valid artifact"),
                clock=ManualClock(),
            ).capture(FRAME_A, binding=BINDING)
        self.assertEqual(invalid_id.exception.code, "artifact_publish_failed")

        with self.assertRaises(ScreenshotError) as arbitrary_path:
            store.capture_from_path("/tmp/should-not-be-read", binding=BINDING)
        self.assertEqual(arbitrary_path.exception.code, "path_input_forbidden")

    def test_redaction_and_boundary_rejections_happen_without_publication(self) -> None:
        transport = RecordingTransport()
        store = ScreenshotStore(transport, clock=ManualClock())

        withheld = store.capture(
            b"not an image and must not be parsed",
            binding=BINDING,
            sensitive=True,
            redaction_reason="private_surface",
        )
        self.assertFalse(withheld.captured)
        self.assertEqual(withheld.omission.reason, "private_surface")
        self.assertEqual(withheld.omission.redaction_state, "withheld")
        self.assertEqual(len(transport.calls), 0)

        with self.assertRaises(ScreenshotError) as empty:
            store.capture(b"", binding=BINDING)
        self.assertEqual(empty.exception.code, "invalid_screenshot")
        with self.assertRaises(ScreenshotError) as invalid_type:
            store.capture("/tmp/not-input-bytes", binding=BINDING)
        self.assertEqual(invalid_type.exception.code, "invalid_screenshot")
        with self.assertRaises(ScreenshotError) as too_large:
            ScreenshotStore(
                RecordingTransport(),
                limits=_limits(max_frame_encoded_bytes=len(FRAME_A), max_retained_bytes=len(FRAME_A)),
                clock=ManualClock(),
            ).capture(FRAME_A + b"x", binding=BINDING)
        self.assertEqual(too_large.exception.code, "frame_encoded_byte_budget")

        pixel_limited = ScreenshotStore(
            RecordingTransport(),
            limits=_limits(max_frame_pixels=1),
            clock=ManualClock(),
        ).capture(_png(2, 2), binding=BINDING)
        self.assertFalse(pixel_limited.captured)
        self.assertEqual(pixel_limited.omission.reason, "frame_pixel_budget")
        decoded_limited = ScreenshotStore(
            RecordingTransport(),
            limits=_limits(max_frame_decoded_bytes=3),
            clock=ManualClock(),
        ).capture(FRAME_A, binding=BINDING)
        self.assertFalse(decoded_limited.captured)
        self.assertEqual(decoded_limited.omission.reason, "frame_decoded_byte_budget")

        corrupt = bytearray(FRAME_A)
        corrupt[-5] ^= 1
        with self.assertRaises(ScreenshotError) as bad_png:
            store.capture(bytes(corrupt), binding=BINDING)
        self.assertEqual(bad_png.exception.code, "invalid_screenshot")

    def test_png_jpeg_gif_and_webp_fixtures_are_dimension_checked(self) -> None:
        fixtures = (
            ("image/png", _png(2, 3)),
            ("image/jpeg", _jpeg(2, 3)),
            ("image/gif", _gif(2, 3)),
            ("image/webp", _webp(2, 3)),
        )
        store = ScreenshotStore(RecordingTransport(), clock=ManualClock())
        for mime_type, data in fixtures:
            result = store.capture(data, binding=BINDING, mime_type=mime_type)
            self.assertTrue(result.captured)
            assert result.reference is not None
            self.assertEqual(result.reference.mime_type, mime_type)
            self.assertEqual(result.reference.dimensions, (2, 3))
            self.assertEqual(result.reference.pixel_count, 6)

        with self.assertRaises(ScreenshotError) as unsupported:
            store.capture(FRAME_A, binding=BINDING, mime_type="image/bmp")
        self.assertEqual(unsupported.exception.code, "unsupported_mime")

    def test_projection_selects_only_fresh_and_explicit_comparison_references(self) -> None:
        store = ScreenshotStore(RecordingTransport(), clock=ManualClock())
        first = store.capture(FRAME_A, binding=BINDING)
        second = store.capture(FRAME_B, binding=BINDING)
        assert first.reference is not None and second.reference is not None

        comparison = store.project(
            binding=BINDING,
            fresh=second,
            selected_frame_ids=[first.reference],
            text="compare current with prior",
        )
        self.assertEqual(comparison.selected_frame_ids, (second.frame_id, first.frame_id))
        self.assertEqual(comparison.selected_image_count, 2)
        self.assertEqual(comparison.omitted_historical_count, 0)
        self.assertEqual(len(comparison.image_parts), 2)
        self.assertEqual(
            [part["artifact_id"] for part in comparison.image_parts],
            [second.artifact_id, first.artifact_id],
        )
        self.assertEqual(len(comparison.frame_metadata), 2)
        _assert_no_data_key(comparison.to_tool_result(), self)

        no_comparison = store.project(binding=BINDING, fresh=second)
        self.assertEqual(no_comparison.selected_frame_ids, (second.frame_id,))
        self.assertEqual(no_comparison.omitted_historical_count, 1)
        self.assertEqual(len(no_comparison.image_parts), 1)

        with self.assertRaises(FreshObservationRequired) as stale:
            store.require_fresh_observation(first, binding=BINDING)
        self.assertEqual(stale.exception.code, "fresh_observation_required")
        current = store.require_fresh_observation(second, binding=BINDING)
        self.assertEqual(current.frame_id, second.frame_id)
        self.assertFalse(current.action_eligible)

        duplicate = store.project(binding=BINDING, fresh=second, selected_frame_ids=[second, second])
        self.assertEqual(duplicate.selected_image_count, 1)
        self.assertTrue(any(item.reason == "fresh_frame_already_selected" for item in duplicate.omissions))
        invalid = store.project(binding=BINDING, fresh=second, selected_frame_ids=["frame_invalid"])
        self.assertTrue(any(item.reason == "invalid_frame_reference" for item in invalid.omissions))

    def test_request_and_retry_budgets_remove_images_before_transport(self) -> None:
        transport = RecordingTransport()
        store = ScreenshotStore(transport, clock=ManualClock())
        result = store.capture(FRAME_A, binding=BINDING)
        assert result.reference is not None
        baseline = store.project(binding=BINDING, fresh=result)
        self.assertGreater(baseline.request_encoded_bytes, baseline.base64_bytes)
        self.assertEqual(baseline.base64_bytes, (len(FRAME_A) + 2) // 3 * 4)
        self.assertEqual(baseline.to_request()["content"][1]["artifact_id"], result.artifact_id)

        denied_budget = RequestBudget(
            max_encoded_bytes=baseline.request_encoded_bytes - 1,
            max_cumulative_encoded_bytes=baseline.request_encoded_bytes,
        )
        denied = store.project(binding=BINDING, fresh=result, request_budget=denied_budget)
        self.assertFalse(denied.request_admitted)
        self.assertEqual(denied.request_admission_reason, "request_encoded_byte_budget")
        self.assertEqual(denied.image_parts, ())
        self.assertEqual(denied.attempt.encoded_bytes, baseline.request_encoded_bytes)
        with self.assertRaises(ScreenshotError):
            denied.to_request()

        retry_budget = RequestBudget(
            max_encoded_bytes=baseline.request_encoded_bytes,
            max_cumulative_encoded_bytes=baseline.request_encoded_bytes * 2,
            max_attempts=2,
            allow_retries=True,
        )
        first_attempt = store.project(binding=BINDING, fresh=result, request_budget=retry_budget)
        self.assertTrue(first_attempt.attempt.admitted)
        unauthorized_retry = store.project(
            binding=BINDING,
            fresh=result,
            request_budget=retry_budget,
            retry=True,
            host_recovery=False,
        )
        self.assertFalse(unauthorized_retry.request_admitted)
        self.assertEqual(unauthorized_retry.request_admission_reason, "retry_not_authorized")
        self.assertEqual(retry_budget.attempts, 1)
        authorized_retry = store.project(
            binding=BINDING,
            fresh=result,
            request_budget=retry_budget,
            retry=True,
            host_recovery=True,
        )
        self.assertTrue(authorized_retry.request_admitted)
        self.assertEqual(retry_budget.attempts, 2)
        self.assertEqual(retry_budget.cumulative_encoded_bytes, baseline.request_encoded_bytes * 2)

        cumulative_budget = RequestBudget(
            max_encoded_bytes=baseline.request_encoded_bytes,
            max_cumulative_encoded_bytes=baseline.request_encoded_bytes * 2 - 1,
            max_attempts=3,
            allow_retries=True,
        )
        self.assertTrue(store.project(binding=BINDING, fresh=result, request_budget=cumulative_budget).request_admitted)
        cumulative_denied = store.project(
            binding=BINDING,
            fresh=result,
            request_budget=cumulative_budget,
            retry=True,
            host_recovery=True,
        )
        self.assertFalse(cumulative_denied.request_admitted)
        self.assertEqual(cumulative_denied.request_admission_reason, "cumulative_request_encoded_byte_budget")
        self.assertEqual(cumulative_budget.attempts, 1)

    def test_host_artifact_adapter_uses_inline_or_private_staging_and_cleans_up(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            publisher = RecordingPublisher(root)
            staged = HostArtifactTransport(publisher, scratch_directory=root, inline_limit=1)
            artifact_id = staged.publish(
                mime_type="image/png",
                data=FRAME_A,
                size=len(FRAME_A),
                sha256=hashlib.sha256(FRAME_A).hexdigest(),
            )
            self.assertEqual(artifact_id, "host-artifact-1")
            self.assertEqual(len(publisher.calls), 1)
            self.assertIn("path", publisher.calls[0])
            self.assertEqual(publisher.path_bytes, [FRAME_A])
            stage_directory = root / HostArtifactTransport._STAGE_DIRECTORY
            self.assertEqual(list(stage_directory.iterdir()), [])

            inline_publisher = RecordingPublisher(root)
            inline = HostArtifactTransport(inline_publisher, scratch_directory=root, inline_limit=len(FRAME_A))
            inline.publish(
                mime_type="image/png",
                data=FRAME_A,
                size=len(FRAME_A),
                sha256=hashlib.sha256(FRAME_A).hexdigest(),
            )
            self.assertIn("data", inline_publisher.calls[0])
            self.assertNotIn("path", inline_publisher.calls[0])

            failing_publisher = RecordingPublisher(root, failure=RuntimeError("host unavailable"))
            failing = HostArtifactTransport(failing_publisher, scratch_directory=root, inline_limit=1)
            with self.assertRaises(ArtifactTransportError) as failure:
                failing.publish(
                    mime_type="image/png",
                    data=FRAME_A,
                    size=len(FRAME_A),
                    sha256=hashlib.sha256(FRAME_A).hexdigest(),
                )
            self.assertEqual(failure.exception.code, "artifact_publish_failed")
            self.assertEqual(list(stage_directory.iterdir()), [])

            interrupted = stage_directory / "screen-stage-interrupted.bin"
            interrupted.write_bytes(FRAME_A)
            staged.cleanup()
            self.assertFalse(interrupted.exists())

    def test_host_artifact_adapter_fails_closed_for_negotiation_and_unsafe_roots(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            no_artifacts = HostArtifactTransport(
                RecordingPublisher(root, features=set()),
                scratch_directory=root,
                inline_limit=1,
            )
            with self.assertRaises(ArtifactTransportError) as unavailable:
                no_artifacts.publish(
                    mime_type="image/png",
                    data=FRAME_A,
                    size=len(FRAME_A),
                    sha256=hashlib.sha256(FRAME_A).hexdigest(),
                )
            self.assertEqual(unavailable.exception.code, "artifacts_unavailable")

            relative = HostArtifactTransport(RecordingPublisher(root), scratch_directory=Path("relative"), inline_limit=1)
            with self.assertRaises(ArtifactTransportError) as relative_error:
                relative.publish(
                    mime_type="image/png",
                    data=FRAME_A,
                    size=len(FRAME_A),
                    sha256=hashlib.sha256(FRAME_A).hexdigest(),
                )
            self.assertEqual(relative_error.exception.code, "unsafe_artifact_path")

            symlink = root / "scratch-link"
            actual = root / "actual"
            actual.mkdir()
            symlink.symlink_to(actual, target_is_directory=True)
            linked = HostArtifactTransport(RecordingPublisher(actual), scratch_directory=symlink, inline_limit=1)
            with self.assertRaises(ArtifactTransportError) as symlink_error:
                linked.publish(
                    mime_type="image/png",
                    data=FRAME_A,
                    size=len(FRAME_A),
                    sha256=hashlib.sha256(FRAME_A).hexdigest(),
                )
            self.assertEqual(symlink_error.exception.code, "unsafe_artifact_path")

            bad_digest = HostArtifactTransport(RecordingPublisher(root), scratch_directory=root)
            with self.assertRaises(ArtifactTransportError) as digest_error:
                bad_digest.publish(
                    mime_type="image/png",
                    data=FRAME_A,
                    size=len(FRAME_A),
                    sha256="0" * 64,
                )
            self.assertEqual(digest_error.exception.code, "artifact_integrity_failed")

    def test_retention_pinning_recovery_compaction_and_settlement_are_bounded(self) -> None:
        clock = ManualClock()
        transport = RecordingTransport()
        store = ScreenshotStore(
            transport,
            limits=_limits(max_retention_seconds=5, max_history_entries=3, max_frames=4),
            clock=clock,
        )
        first = store.capture(FRAME_A, binding=BINDING)
        second = store.capture(FRAME_B, binding=BINDING)
        assert first.reference is not None and second.reference is not None
        self.assertTrue(store.pin(first.reference, binding=BINDING))

        clock.value = 6
        self.assertEqual(store.latest(BINDING).frame_id, first.frame_id)
        pinned_cleanup = store.cleanup(now=6, binding=BINDING)
        self.assertEqual(pinned_cleanup.skipped_pinned, 1)
        self.assertEqual(store.get(first.reference, binding=BINDING).retention_state, "retained")

        forged = replace(first.reference, artifact_id="forged-artifact")
        store.unpin(forged, binding=BINDING)
        self.assertEqual(store.cleanup(now=6, binding=BINDING).skipped_pinned, 1)
        store.unpin(first.reference, binding=BINDING)
        self.assertEqual(store.cleanup(now=6, binding=BINDING).expired, 1)
        self.assertEqual(store.latest(BINDING), None)
        self.assertEqual(store.get(first.reference, binding=BINDING).retention_state, "expired")

        recover_clock = ManualClock()
        recover_store = ScreenshotStore(RecordingTransport(), clock=recover_clock)
        missing = recover_store.capture(FRAME_A, binding=BINDING)
        corrupt = recover_store.capture(FRAME_B, binding=BINDING)
        assert missing.reference is not None and corrupt.reference is not None
        report = recover_store.recover(
            verifier=lambda reference: "corrupt" if reference.frame_id == corrupt.frame_id else False,
        )
        self.assertEqual(report.checked, 2)
        self.assertEqual(report.missing, 1)
        self.assertEqual(report.corrupt, 1)
        self.assertEqual(recover_store.get(missing.reference, binding=BINDING).retention_state, "missing")
        self.assertEqual(recover_store.get(corrupt.reference, binding=BINDING).retention_state, "corrupt")
        self.assertEqual(recover_store.recover().checked, 0)

        compact = ScreenshotStore(
            RecordingTransport(),
            limits=_limits(max_frames=3, max_history_entries=3, max_selected_images=3),
            clock=ManualClock(),
        )
        for _ in range(3):
            self.assertTrue(compact.capture(FRAME_A, binding=BINDING).captured)
        omitted = compact.capture(FRAME_B, binding=BINDING)
        self.assertEqual(omitted.omission.reason, "frame_count_budget")
        self.assertEqual(len(compact.history(binding=BINDING)), 3)
        _assert_no_data_key(compact.canonical_history(binding=BINDING), self)

        settled = ScreenshotStore(RecordingTransport(), clock=ManualClock())
        settled.capture(FRAME_A, binding=BINDING)
        settle_report = settled.settle()
        self.assertEqual(settle_report["frames"], 1)
        self.assertTrue(settled.metrics()["settled"])
        after_settle = settled.capture(FRAME_A, binding=BINDING)
        self.assertFalse(after_settle.captured)
        self.assertEqual(after_settle.omission.reason, "generation_settled")
        self.assertEqual(settled.metrics()["admitted_frame_count"], 1)


if __name__ == "__main__":
    unittest.main()
