"""Behavioural tests for the bounded, host-mediated extension event bus.

The bus is fail-closed and bounded by construction: unknown or foreign topics,
payloads that do not match the declared topic spec, credential/PII/path-shaped
data, and queue pressure are all refused instead of silently degraded. These
tests exercise the SDK enforcement kernel plus the extension-side participant
with an injected host-request callable. Real Rust process/product fixtures are
separate; their execution is parent-owned and is not implied by SDK checks.
"""

from __future__ import annotations

import sys
import threading
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from octet_extension.event_bus import (  # noqa: E402
    CAPABILITY_MISMATCH,
    DEFAULT_LIMITS,
    INVALID_PARAMS,
    RESOURCE_EXHAUSTED,
    BusEnvelope,
    BusError,
    BusLimits,
    BoundedQueue,
    EventBusKernel,
    FieldSpec,
    HostEventBus,
    TopicRegistry,
    TopicSpec,
    validate_payload,
    validate_topic,
)
from octet_extension.protocol import RpcError  # noqa: E402


BINDING_ID = "host-binding-1"


def bind(bus: HostEventBus, binding_id: str = BINDING_ID, revision: int = 1) -> HostEventBus:
    """Deliver the host's binding notice, which every operation now requires."""
    assert bus.accept_lifecycle(
        {"kind": "binding", "binding_id": binding_id, "binding_revision": revision}
    )
    return bus


def status_topic(owner: str = "alpha") -> TopicSpec:
    return TopicSpec(
        owner=owner,
        name="status",
        fields=(
            FieldSpec.string("summary", max_bytes=128),
            FieldSpec.integer("count", minimum=0, maximum=1000),
            FieldSpec.boolean("degraded", required=False),
            FieldSpec.enum("phase", ("starting", "ready", "failed")),
        ),
        description="bounded extension status",
    )


class TopicValidationTests(unittest.TestCase):
    def test_topic_syntax_is_strict_and_namespaced(self) -> None:
        self.assertEqual(("alpha", "status"), validate_topic("bus.alpha.status"))
        for topic in ("", "alpha.status", "bus.alpha", "bus.Alpha.status", "bus.alpha.status.extra", "bus../etc"):
            with self.assertRaises(BusError, msg=topic) as caught:
                validate_topic(topic)
            self.assertEqual(INVALID_PARAMS, caught.exception.code)

    def test_unknown_topic_fails_closed(self) -> None:
        kernel = EventBusKernel()
        kernel.declare(status_topic())
        with self.assertRaises(BusError) as caught:
            kernel.registry.get("bus.alpha.missing")
        self.assertEqual("unknown_topic", caught.exception.reason)
        with self.assertRaises(BusError):
            kernel.subscribe("beta", "bus.alpha.missing")

    def test_registry_rejects_duplicate_and_forbidden_declarations(self) -> None:
        registry = TopicRegistry()
        registry.declare(status_topic())
        with self.assertRaises(BusError) as caught:
            registry.declare(status_topic())
        self.assertEqual("topic_already_declared", caught.exception.reason)
        with self.assertRaises(BusError) as forbidden:
            registry.declare(
                TopicSpec(owner="alpha", name="audit", fields=(FieldSpec.string("apiKey"),))
            )
        self.assertEqual("forbidden_field", forbidden.exception.reason)

    def test_limits_are_validated(self) -> None:
        with self.assertRaises(BusError):
            BusLimits(max_queue_messages=0)
        with self.assertRaises(BusError):
            BusLimits(max_queue_messages=8192)
        with self.assertRaises(BusError):
            BusLimits(max_message_bytes=-1)


class PayloadValidationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.spec = status_topic()

    def test_valid_payload_is_returned_unchanged(self) -> None:
        payload = {"summary": "all green", "count": 3, "phase": "ready"}
        self.assertEqual(payload, validate_payload(self.spec, payload))

    def test_shape_violations_fail_closed(self) -> None:
        cases = (
            ({"count": 1, "phase": "ready"}, "missing_field"),
            ({"summary": "x", "count": 1, "phase": "ready", "extra": "nope"}, "unknown_field"),
            ({"summary": "x", "count": "1", "phase": "ready"}, "invalid_field_type"),
            ({"summary": "x", "count": 1, "phase": "unknown"}, "invalid_field_value"),
            ({"summary": "x", "count": -1, "phase": "ready"}, "field_below_minimum"),
            ({"summary": "x", "count": 5000, "phase": "ready"}, "field_above_maximum"),
            ("not-an-object", "payload_not_an_object"),
        )
        for payload, reason in cases:
            with self.assertRaises(BusError, msg=reason) as caught:
                validate_payload(self.spec, payload)
            self.assertEqual(reason, caught.exception.reason)
            self.assertEqual(INVALID_PARAMS, caught.exception.code)

    def test_authority_shaped_fields_are_refused(self) -> None:
        for field_name in ("authorization", "api_key", "privatePath", "capability", "sessionToken", "trust"):
            spec = TopicSpec(owner="alpha", name="audit", fields=(FieldSpec.string(field_name),))
            with self.assertRaises(BusError) as caught:
                validate_payload(spec, {field_name: "value"})
            self.assertEqual("forbidden_field", caught.exception.reason)

    def test_pii_and_private_values_are_refused(self) -> None:
        cases = (
            ("user@example.com", "pii_detected"),
            ("sk-live-abcdefghijklmnop", "pii_detected"),
            ("Bearer abcdefghijklmnop", "pii_detected"),
            ("/Users/someone/private/report.txt", "private_path"),
            ("~/.ssh/id_ed25519", "private_path"),
            ("+1 (555) 010-9999", "pii_detected"),
            ("first line\nsecond line", "control_character"),
        )
        for value, reason in cases:
            with self.assertRaises(BusError, msg=value) as caught:
                validate_payload(self.spec, {"summary": value, "count": 1, "phase": "ready"})
            self.assertEqual(reason, caught.exception.reason)

    def test_oversized_string_and_field_count_are_bounded(self) -> None:
        with self.assertRaises(BusError) as caught:
            validate_payload(self.spec, {"summary": "x" * 129, "count": 1, "phase": "ready"})
        self.assertEqual(RESOURCE_EXHAUSTED, caught.exception.code)
        spec = TopicSpec(
            owner="alpha",
            name="wide",
            fields=tuple(FieldSpec.string("field{0}".format(index)) for index in range(3)),
        )
        limits = BusLimits(max_payload_fields=2)
        with self.assertRaises(BusError) as caught:
            validate_payload(spec, {"field0": "a", "field1": "b", "field2": "c"}, limits)
        self.assertEqual("too_many_fields", caught.exception.reason)


class BoundedQueueTests(unittest.TestCase):
    def envelope(self, sequence: int) -> BusEnvelope:
        return BusEnvelope(
            topic="bus.alpha.status",
            publisher="alpha",
            sequence=sequence,
            published_at_ms=sequence,
            payload={"sequence": sequence},
            byte_len=16,
        )

    def test_queue_bounds_messages_and_bytes(self) -> None:
        queue = BoundedQueue(BusLimits(max_queue_messages=2, max_queue_bytes=1024))
        queue.push(self.envelope(1))
        queue.push(self.envelope(2))
        with self.assertRaises(BusError) as caught:
            queue.push(self.envelope(3))
        self.assertEqual(RESOURCE_EXHAUSTED, caught.exception.code)
        self.assertEqual(2, len(queue))
        drained = queue.drain()
        self.assertEqual([1, 2], [item.sequence for item in drained])
        self.assertEqual(0, queue.byte_len)

    def test_queue_byte_budget_and_drain_bound(self) -> None:
        queue = BoundedQueue(BusLimits(max_queue_messages=64, max_queue_bytes=20))
        queue.push(self.envelope(1))
        with self.assertRaises(BusError) as caught:
            queue.push(self.envelope(2))
        self.assertEqual("queue_bytes_exceeded", caught.exception.reason)
        self.assertEqual(1, len(queue.drain(max_messages=1)))
        with self.assertRaises(BusError):
            queue.drain(max_messages=0)


class KernelTests(unittest.TestCase):
    def setUp(self) -> None:
        self.kernel = EventBusKernel()
        self.status = self.kernel.declare(status_topic("alpha"))

    def publish(self, publisher: str = "alpha", *, sequence: int = 1, payload=None) -> BusEnvelope:
        return self.kernel.publish(
            publisher,
            self.status.topic,
            payload if payload is not None else {"summary": "ok", "count": 1, "phase": "ready"},
            published_at_ms=1_000 * sequence,
        )

    def test_publish_requires_topic_ownership(self) -> None:
        with self.assertRaises(BusError) as caught:
            self.publish("beta")
        self.assertEqual(CAPABILITY_MISMATCH, caught.exception.code)
        self.assertEqual("foreign_topic", caught.exception.reason)

    def test_delivery_is_per_extension_and_subscription_scoped(self) -> None:
        self.kernel.subscribe("alpha", self.status.topic)
        self.kernel.subscribe("beta", self.status.topic)
        envelope = self.publish()
        self.assertEqual(("alpha", "beta"), self.kernel.subscribers(self.status.topic))
        self.assertEqual([envelope], self.kernel.deliver("beta", self.status.topic))
        self.assertEqual([envelope], self.kernel.deliver("alpha", self.status.topic))
        self.assertEqual([], self.kernel.deliver("beta", self.status.topic))
        with self.assertRaises(BusError) as caught:
            self.kernel.deliver("gamma", self.status.topic)
        self.assertEqual("not_subscribed", caught.exception.reason)

    def test_unsubscribed_extension_receives_nothing(self) -> None:
        self.kernel.subscribe("alpha", self.status.topic)
        self.publish()
        self.kernel.publish(
            "alpha",
            self.status.topic,
            {"summary": "second", "count": 2, "phase": "ready"},
            published_at_ms=2_000,
        )
        self.assertEqual(2, len(self.kernel.deliver("alpha", self.status.topic)))
        self.kernel.unsubscribe("alpha", self.status.topic)
        with self.assertRaises(BusError):
            self.kernel.deliver("alpha", self.status.topic)

    def test_sequence_is_monotonic_across_publisher_topics(self) -> None:
        other = self.kernel.declare(TopicSpec("alpha", "other", (FieldSpec.boolean("ready"),)))
        self.kernel.subscribe("beta", self.status.topic)
        self.kernel.subscribe("beta", other.topic)
        first = self.publish(sequence=1)
        second = self.kernel.publish("alpha", other.topic, {"ready": True}, published_at_ms=2_000)
        third = self.publish(sequence=3)
        self.assertEqual((1, 2, 3), (first.sequence, second.sequence, third.sequence))
        self.assertEqual([1, 3], [item.sequence for item in self.kernel.deliver("beta", self.status.topic)])
        self.assertEqual([2], [item.sequence for item in self.kernel.deliver("beta", other.topic)])
        # A different publisher begins its own process-scoped counter.
        beta = self.kernel.declare(TopicSpec("beta", "other", (FieldSpec.boolean("ready"),)))
        self.assertEqual(1, self.kernel.publish("beta", beta.topic, {"ready": True}, published_at_ms=4_000).sequence)

    def test_full_queue_raises_instead_of_dropping(self) -> None:
        kernel = EventBusKernel(BusLimits(max_queue_messages=1))
        status = kernel.declare(status_topic("alpha"))
        kernel.subscribe("beta", status.topic)
        kernel.publish("alpha", status.topic, {"summary": "a", "count": 1, "phase": "ready"}, published_at_ms=1)
        with self.assertRaises(BusError) as caught:
            kernel.publish("alpha", status.topic, {"summary": "b", "count": 2, "phase": "ready"}, published_at_ms=2)
        self.assertEqual(RESOURCE_EXHAUSTED, caught.exception.code)
        self.assertEqual(1, len(kernel.deliver("beta", status.topic)))

    def test_message_size_and_subscription_limits(self) -> None:
        kernel = EventBusKernel(BusLimits(max_message_bytes=32))
        status = kernel.declare(status_topic("alpha"))
        kernel.subscribe("beta", status.topic)
        with self.assertRaises(BusError) as caught:
            kernel.publish(
                "alpha",
                status.topic,
                {"summary": "x" * 64, "count": 1, "phase": "ready"},
                published_at_ms=1,
            )
        self.assertEqual(RESOURCE_EXHAUSTED, caught.exception.code)

        limited = EventBusKernel(BusLimits(max_subscriptions=1))
        limited.declare(status_topic("alpha"))
        limited.declare(TopicSpec(owner="alpha", name="other", fields=(FieldSpec.string("note"),)))
        limited.subscribe("beta", "bus.alpha.status")
        with self.assertRaises(BusError) as caught:
            limited.subscribe("beta", "bus.alpha.other")
        self.assertEqual("subscription_limit", caught.exception.reason)

    def test_expired_messages_are_dropped_at_delivery(self) -> None:
        self.kernel.subscribe("beta", self.status.topic)
        self.publish(sequence=1)
        self.assertEqual([], self.kernel.deliver("beta", self.status.topic, now_ms=1_000_000))
        self.kernel.publish(
            "alpha",
            self.status.topic,
            {"summary": "fresh", "count": 2, "phase": "ready"},
            published_at_ms=1_000_000,
        )
        self.assertEqual(1, len(self.kernel.deliver("beta", self.status.topic, now_ms=1_000_100)))

    def test_envelope_is_inert_and_frozen(self) -> None:
        self.kernel.subscribe("beta", self.status.topic)
        envelope = self.publish()
        with self.assertRaises(Exception):
            envelope.publisher = "beta"  # type: ignore[misc]
        payload = envelope.payload
        with self.assertRaises(TypeError):
            payload["count"] = 5  # type: ignore[index]
        self.assertEqual(
            {
                "topic": "bus.alpha.status",
                "publisher": "alpha",
                "sequence": 1,
                "published_at_ms": 1000,
                "binding_id": "",
                "publisher_instance_id": "", "process_generation": 0,
                "payload": {"summary": "ok", "count": 1, "phase": "ready"},
            },
            envelope.public(),
        )


class HostEventBusClientTests(unittest.TestCase):
    def setUp(self) -> None:
        self.registry = TopicRegistry()
        self.registry.declare(status_topic("alpha"))
        self.registry.declare(status_topic("beta"))
        self.calls = []

    def make_bus(self, extension_id: str, responder=None) -> HostEventBus:
        def request(method, params, cancelled):
            self.calls.append((method, params))
            if responder is not None:
                return responder(method, params, cancelled)
            if method == "bus/publish":
                return {"binding_id": BINDING_ID, "sequence": 7, "published_at_ms": 5000}
            if method == "bus/subscribe":
                return {
                    "state": "active",
                    "binding_id": BINDING_ID,
                    "topic_revision": 1,
                    "publisher_instance_id": "instance-alpha",
                    "process_generation": 1,
                }
            return {"binding_id": BINDING_ID}

        return bind(
            HostEventBus(
                request,
                self.registry,
                extension_id=extension_id,
                now_ms=lambda: 5_000,
            )
        )

    def test_subscribe_and_publish_use_the_host_methods(self) -> None:
        bus = self.make_bus("alpha")
        bus.subscribe("bus.beta.status")
        envelope = bus.publish(
            "bus.alpha.status",
            {"summary": "ok", "count": 2, "phase": "ready"},
        )
        self.assertEqual(7, envelope.sequence)
        self.assertEqual(
            [
                ("bus/subscribe", {"topic": "bus.beta.status", "binding_id": BINDING_ID}),
                (
                    "bus/publish",
                    {
                        "topic": "bus.alpha.status",
                        "payload": {"summary": "ok", "count": 2, "phase": "ready"},
                        "binding_id": BINDING_ID,
                    },
                ),
            ],
            self.calls,
        )

    def test_local_violations_never_reach_the_host(self) -> None:
        bus = self.make_bus("alpha")
        with self.assertRaises(BusError):
            bus.publish("bus.beta.status", {"summary": "ok", "count": 1, "phase": "ready"})
        with self.assertRaises(BusError):
            bus.publish("bus.alpha.status", {"summary": "user@example.com", "count": 1, "phase": "ready"})
        with self.assertRaises(BusError):
            bus.publish("bus.alpha.status", {"summary": "ok", "count": 1, "phase": "nope"})
        with self.assertRaises(BusError):
            bus.subscribe("bus.alpha.missing")
        self.assertEqual([], self.calls)

    def test_host_failure_is_propagated_not_swallowed(self) -> None:
        def failing(method, params, cancelled):
            raise RpcError(-32601, "unknown or unnegotiated method")

        bus = self.make_bus("alpha", failing)
        with self.assertRaises(RpcError) as caught:
            bus.publish("bus.alpha.status", {"summary": "ok", "count": 1, "phase": "ready"})
        self.assertEqual(-32601, caught.exception.code)
        self.assertFalse(bus._subscribed)  # type: ignore[attr-defined]

    def test_inbound_delivery_is_validated_and_sequence_bounded(self) -> None:
        bus = self.make_bus("beta")
        bus.subscribe("bus.alpha.status")
        envelope = bus.accept_event(
            {
                "topic": "bus.alpha.status",
                "publisher": "alpha",
                "sequence": 3,
                "published_at_ms": 4_900,
                "binding_id": BINDING_ID,
                "publisher_instance_id": "instance-alpha", "process_generation": 1,
                "payload": {"summary": "ok", "count": 1, "phase": "ready"},
            }
        )
        assert envelope is not None
        self.assertEqual(("alpha", 3), (envelope.publisher, envelope.sequence))

    def test_inbound_violations_are_refused(self) -> None:
        bus = self.make_bus("beta")
        bus.subscribe("bus.alpha.status")
        base = {
            "topic": "bus.alpha.status",
            "publisher": "alpha",
            "sequence": 1,
            "published_at_ms": 4_900,
                "binding_id": BINDING_ID,
                "publisher_instance_id": "instance-alpha", "process_generation": 1,
            "payload": {"summary": "ok", "count": 1, "phase": "ready"},
        }
        cases = (
            ({**base, "topic": "bus.beta.status"}, CAPABILITY_MISMATCH),
            ({**base, "publisher": "beta"}, INVALID_PARAMS),
            ({**base, "publisher": "beta"}, INVALID_PARAMS),
            ({**base, "sequence": "1"}, INVALID_PARAMS),
            ({**base, "payload": {"summary": "user@example.com", "count": 1, "phase": "ready"}}, INVALID_PARAMS),
            ({**base, "payload": {"summary": "ok", "count": 1}}, INVALID_PARAMS),
            ("not-an-object", INVALID_PARAMS),
        )
        for params, code in cases:
            with self.assertRaises(BusError, msg=repr(params)) as caught:
                bus.accept_event(params)
            self.assertEqual(code, caught.exception.code)

    def test_inbound_stale_sequence_is_refused(self) -> None:
        bus = self.make_bus("beta")
        bus.subscribe("bus.alpha.status")
        bus.accept_event(
            {
                "topic": "bus.alpha.status",
                "publisher": "alpha",
                "sequence": 2,
                "published_at_ms": 4_900,
                "binding_id": BINDING_ID,
                "publisher_instance_id": "instance-alpha", "process_generation": 1,
                "payload": {"summary": "ok", "count": 1, "phase": "ready"},
            }
        )
        with self.assertRaises(BusError) as caught:
            bus.accept_event(
                {
                    "topic": "bus.alpha.status",
                    "publisher": "alpha",
                    "sequence": 1,
                    "published_at_ms": 4_900,
                    "binding_id": BINDING_ID,
                "publisher_instance_id": "instance-alpha", "process_generation": 1,
                    "payload": {"summary": "ok", "count": 1, "phase": "ready"},
                }
            )
        self.assertEqual("stale_sequence", caught.exception.reason)

    def test_pending_ack_overtaken_by_availability_retries_without_losing_interest(self) -> None:
        topic = "bus.alpha.status"
        calls = []

        def responder(method, params, cancelled):
            calls.append(method)
            if len(calls) == 1:
                self.assertTrue(bus.accept_lifecycle({
                    "kind": "topic_available", "binding_id": BINDING_ID, "topic": topic,
                    "topic_revision": 1, "publisher_instance_id": "instance-alpha", "process_generation": 1,
                }))
                return {"state": "pending", "binding_id": BINDING_ID, "topic_revision": 0}
            return {"state": "active", "binding_id": BINDING_ID, "topic_revision": 1,
                    "publisher_instance_id": "instance-alpha", "process_generation": 1}

        bus = self.make_bus("beta", responder)
        try:
            bus.subscribe(topic)
            self.assertTrue(bus.wait_rebound(2))
            self.assertEqual(["bus/subscribe", "bus/subscribe"], calls)
            self.assertEqual([topic], bus.snapshot()["subscribed"])
            self.assertEqual([], bus.snapshot()["pending"])
        finally:
            bus.close()

    def test_stale_active_ack_never_resurrects_replaced_publisher(self) -> None:
        topic = "bus.alpha.status"
        calls = []

        def responder(method, params, cancelled):
            calls.append(method)
            if len(calls) == 1:
                for kind, revision, instance in (
                    ("topic_unavailable", 2, "old-instance"),
                    ("topic_available", 3, "new-instance"),
                ):
                    bus.accept_lifecycle({
                        "kind": kind, "binding_id": BINDING_ID, "topic": topic,
                        "topic_revision": revision, "publisher_instance_id": instance, "process_generation": 1,
                    })
                return {"state": "active", "binding_id": BINDING_ID, "topic_revision": 1,
                        "publisher_instance_id": "old-instance", "process_generation": 1}
            return {"state": "active", "binding_id": BINDING_ID, "topic_revision": 3,
                    "publisher_instance_id": "new-instance", "process_generation": 1}

        bus = self.make_bus("beta", responder)
        try:
            bus.subscribe(topic)
            self.assertTrue(bus.wait_rebound(2))
            self.assertEqual(2, len(calls))
            self.assertEqual(("new-instance", 1), bus._subscribed[topic])
        finally:
            bus.close()

    def test_event_after_active_ack_frame_waits_for_sdk_ack_commit(self) -> None:
        topic = "bus.alpha.status"
        waiting = threading.Event()
        completed = threading.Event()
        accepted = []
        reader = []
        params = {"topic": topic, "publisher": "alpha", "sequence": 1,
                  "published_at_ms": 5_000, "binding_id": BINDING_ID,
                  "publisher_instance_id": "instance-alpha", "process_generation": 1,
                  "payload": {"summary": "safe", "count": 1, "phase": "ready"}}

        def responder(method, request, cancelled):
            def receive():
                try:
                    accepted.append(bus.accept_event(params))
                finally:
                    completed.set()
            reader.append(threading.Thread(target=receive, name="bus-event-reader"))
            reader[0].start()
            self.assertTrue(waiting.wait(2), "event reader should reach the uncommitted ACK")
            return {"state": "active", "binding_id": BINDING_ID, "topic_revision": 1,
                    "publisher_instance_id": "instance-alpha", "process_generation": 1}

        bus = self.make_bus("beta", responder)
        original_wait = bus._condition.wait_for
        def mark_wait(predicate, timeout=None):
            if threading.current_thread().name == "bus-event-reader":
                waiting.set()
            return original_wait(predicate, timeout)
        bus._condition.wait_for = mark_wait
        try:
            bus.subscribe(topic)
            self.assertTrue(completed.wait(2))
            self.assertEqual([1], [event.sequence for event in accepted])
        finally:
            bus.close()
            for thread in reader:
                thread.join(timeout=2)

    def test_declare_is_namespaced_to_the_extension(self) -> None:
        bus = self.make_bus("alpha")
        spec = bus.declare(name="progress", fields=(FieldSpec.integer("percent", minimum=0, maximum=100),))
        self.assertEqual("bus.alpha.progress", spec.topic)
        with self.assertRaises(BusError):
            self.make_bus("gamma").publish("bus.alpha.status", {"summary": "ok", "count": 1, "phase": "ready"})


class ContractStatusTests(unittest.TestCase):
    def test_defaults_are_bounded(self) -> None:
        self.assertLessEqual(DEFAULT_LIMITS.max_message_bytes, 64 * 1024)
        self.assertLessEqual(DEFAULT_LIMITS.max_queue_messages, 256)
        self.assertLessEqual(DEFAULT_LIMITS.max_subscriptions, 32)
        self.assertLessEqual(DEFAULT_LIMITS.max_drain_messages, 256)

    def test_errors_map_to_protocol_codes(self) -> None:
        error = BusError(RESOURCE_EXHAUSTED, "queue_full")
        self.assertEqual({"code": -32012, "reason": "queue_full"}, error.error_object())


class HostMediationRegressions(unittest.TestCase):
    def test_all_limits_can_only_be_lowered(self):
        for name, field in BusLimits.__dataclass_fields__.items():
            with self.assertRaises(BusError):
                BusLimits(**{name: field.default + 1})

    def test_registry_screens_enums_and_rejects_invalid_types_and_bounds(self):
        fields = (
            FieldSpec.enum("summary", ("user@example.com",)),
            FieldSpec("summary", "object"),
            FieldSpec.integer("count", minimum=3, maximum=1),
            FieldSpec.string("summary", max_bytes=1025),
        )
        for field in fields:
            with self.assertRaises(BusError):
                TopicRegistry().declare(TopicSpec("alpha", "status", (field,)))

    def test_queue_pressure_never_partially_fans_out(self):
        kernel = EventBusKernel(BusLimits(max_queue_messages=1))
        kernel.declare(status_topic())
        for peer in ("beta", "gamma"):
            kernel.subscribe(peer, "bus.alpha.status")
        payload = {"summary": "safe", "count": 1, "phase": "ready"}
        kernel.publish("alpha", "bus.alpha.status", payload, published_at_ms=1)
        kernel.deliver("beta", "bus.alpha.status")
        with self.assertRaises(BusError):
            kernel.publish("alpha", "bus.alpha.status", payload, published_at_ms=2)
        self.assertEqual([], kernel.deliver("beta", "bus.alpha.status"))
        kernel.deliver("gamma", "bus.alpha.status")
        self.assertEqual(2, kernel.publish("alpha", "bus.alpha.status", payload, published_at_ms=3).sequence)

    def test_peer_queue_budgets_cover_all_subscribed_topics_before_fanout(self):
        for limits, reason in ((BusLimits(max_queue_messages=1), "queue_full"),
                               (BusLimits(max_queue_bytes=14), "queue_bytes_exceeded")):
            with self.subTest(reason=reason):
                kernel = EventBusKernel(limits)
                for name in ("first", "second"):
                    kernel.declare(TopicSpec(owner="alpha", name=name, fields=(FieldSpec.boolean("ready"),)))
                    kernel.subscribe("beta", "bus.alpha." + name)
                kernel.subscribe("gamma", "bus.alpha.second")
                first = kernel.publish("alpha", "bus.alpha.first", {"ready": True}, published_at_ms=1)
                with self.assertRaises(BusError) as caught:
                    kernel.publish("alpha", "bus.alpha.second", {"ready": True}, published_at_ms=2)
                self.assertEqual(reason, caught.exception.reason)
                self.assertEqual([], kernel.deliver("gamma", "bus.alpha.second"))
                self.assertEqual([first], kernel.deliver("beta", "bus.alpha.first"))
                self.assertEqual(2, kernel.publish("alpha", "bus.alpha.second", {"ready": True}, published_at_ms=3).sequence)

    def test_declaration_is_sent_to_host_and_failed_ack_never_installs_locally(self):
        registry = TopicRegistry()
        calls = []
        bus = bind(HostEventBus(
            lambda method, params, cancelled: (
                calls.append((method, params)),
                {"binding_id": BINDING_ID},
            )[1],
            registry,
            extension_id="alpha",
        ))
        bus.declare(name="status", fields=(FieldSpec.boolean("ready"),))
        self.assertEqual("bus/declare", calls[0][0])
        from octet_extension.api_v03 import BusDeclareParams
        BusDeclareParams.from_wire(calls[0][1])
        def fail(method, params, cancelled):
            raise RpcError(-32601, "unknown or unnegotiated method")
        empty = TopicRegistry()
        bus = bind(HostEventBus(fail, empty, extension_id="alpha"))
        with self.assertRaises(RpcError):
            bus.declare(name="status", fields=(FieldSpec.boolean("ready"),))
        self.assertEqual((), empty.topics())

    def test_generated_bus_wire_refuses_client_identity_and_time(self):
        from octet_extension.api_v03 import BusPublishParams, ContractError
        for key, value in (("publisher", "beta"), ("published_at_ms", 1), ("process_generation", 1)):
            with self.assertRaises(ContractError):
                BusPublishParams.from_wire({"topic": "bus.alpha.status", "payload": {}, key: value})

    def test_false_publish_ack_is_not_reported_as_success(self):
        registry = TopicRegistry()
        registry.declare(status_topic())
        for reply in (
            {},
            {"sequence": 0, "published_at_ms": 1},
            {"sequence": True, "published_at_ms": 1},
        ):
            # A binding-scoped ack must still carry the captured incarnation and
            # a positive integer sequence; anything else is refused.
            bus = bind(HostEventBus(
                lambda method, params, cancelled, reply=reply: {"binding_id": BINDING_ID, **reply},
                registry,
                extension_id="alpha",
            ))
            with self.assertRaises(BusError):
                bus.publish("bus.alpha.status", {"summary": "safe", "count": 1, "phase": "ready"})


if __name__ == "__main__":
    unittest.main()
