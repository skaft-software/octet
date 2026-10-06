from dataclasses import dataclass
import io
from typing import Optional
import unittest

from octet_extension import Extension, Resource, RpcError
from octet_extension.resources import RESOURCE_LIMITS


class ResourceTests(unittest.TestCase):
    def example(self, *, dispose=None):
        ext = Extension(api_version="0.4", stderr=io.StringIO())
        @ext.resource_type("example.Counter.v1", dispose=dispose)
        class Counter:
            def __init__(self, n=0):
                self.n = n
        @dataclass
        class Input:
            n: int = 0
        @dataclass
        class Output:
            counter: Resource[Counter]
        @ext.typed_tool(name="create", description="Create counter", summary=lambda value: "Created.")
        def create(args: Input) -> Output:
            return Output(ext.export(Counter(args.n)))
        @dataclass
        class Use:
            counter: Resource[Counter]
            delta: int = 1
        @ext.typed_tool(name="increment", description="Increment counter", receiver="/counter", summary=lambda value: "Incremented.")
        def increment(args: Use) -> int:
            args.counter.value.n += args.delta
            return args.counter.value.n
        return ext, Counter, Input, Output, Use

    def initialize(self, ext, *, features=True, limits=None):
        value = ext._initialize({"api_version": "0.4", "contributes": {"tools": list(ext._tools)},
            "protocol": {"version": "0.4", "required_features": ["request_cancellation", "content_parts"],
                         "optional_features": ["resource_refs_v1", "operation_descriptors_v1"] if features else [],
                         "limits": {"max_concurrent_requests": 1, "resource_refs_v1": RESOURCE_LIMITS if limits is None else limits}}})
        self.addCleanup(ext._executor.shutdown)
        return value

    def test_generated_nominal_schema_descriptor_and_same_native_object(self):
        ext, Counter, *_ = self.example()
        catalog = self.initialize(ext)
        generated = {tool["name"]: tool for tool in catalog["tools"]}
        self.assertEqual(generated["create"]["operation"], {"id": "create", "resource_inputs": [],
                         "resource_outputs": [{"path": "/counter", "type": "example.Counter.v1"}]})
        self.assertEqual(generated["increment"]["operation"], {"id": "increment", "receiver": "/counter",
                         "resource_inputs": [{"path": "/counter", "type": "example.Counter.v1", "access": "exclusive"}],
                         "resource_outputs": []})
        self.assertEqual(generated["create"]["output_schema"]["properties"]["counter"],
                         generated["increment"]["parameters"]["properties"]["counter"])
        schema = generated["create"]["output_schema"]["properties"]["counter"]
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(schema["properties"]["type"]["enum"], ["example.Counter.v1"])
        calls = []
        def request(method, params, **kwargs):
            calls.append((method, params, kwargs))
            return {"$resource": "host-token", "type": params["type"]}
        ext.request = request
        result = ext._call_tool({"name": "create", "arguments": {"n": 5}})
        self.assertEqual(calls, [("resource/register", {"type": "example.Counter.v1"}, {"operation_scoped": True})])
        reference = result["structured_content"]["counter"]
        for expected in (6, 7):
            value = ext._call_tool({"name": "increment", "arguments": {"counter": reference}})
            self.assertEqual(value["structured_content"], expected)
        self.assertIsInstance(ext._resources.records["host-token"][1], Counter)

    def test_disposal_removes_all_resolution_before_fallible_destructor(self):
        observed, references = [], []
        def dispose(native):
            for ref in references:
                with self.assertRaises(RpcError):
                    _ = ref.value
            observed.append(native.n)
            if native.n == 2:
                raise RuntimeError("destructor failed")
        ext, Counter, *_ = self.example(dispose=dispose)
        self.initialize(ext)
        ext.request = lambda method, params, **kwargs: {"$resource": f"r{len(references)}", "type": params["type"]}
        references.append(ext.export(Counter(1)))
        references.append(ext.export(Counter(2)))
        result = ext._dispatch("resource/dispose", {"resources": [r.to_wire() for r in references], "reason": "retired"})
        self.assertEqual(observed, [1, 2])
        self.assertEqual([r["status"] for r in result["results"]], ["completed", "failed"])
        self.assertEqual(ext._resources.records, {})
        self.assertEqual(ext._resources.identities, set())
        for reference in references:
            with self.assertRaises(RpcError):
                _ = reference.value

    def test_alias_export_and_foreign_registry_refs_are_refused(self):
        ext, Counter, *_ = self.example()
        self.initialize(ext)
        ext.request = lambda method, params, **kwargs: {"$resource": "token", "type": params["type"]}
        value = Counter()
        reference = ext.export(value)
        with self.assertRaises(ValueError):
            ext.export(value)
        with self.assertRaises(RpcError):
            ext._resources.resolve(Resource(reference.token, reference.type, object()))
        for wire in [{"$resource": "unknown", "type": reference.type}, {"$resource": "token", "type": "example.Other.v1"},
                     {**reference.to_wire(), "extra": True}]:
            with self.assertRaises(RpcError):
                ext._call_tool({"name": "increment", "arguments": {"counter": wire}})
        self.assertEqual(value.n, 0)

    def test_resource_opt_in_and_missing_host_support(self):
        ordinary = Extension(api_version="0.4")
        @ordinary.tool(name="ordinary", description="No resource machinery")
        def ordinary_tool(args):
            return "ordinary"
        response = self.initialize(ordinary)
        self.assertNotIn("resource_refs_v1", response["protocol"]["features"])
        self.assertNotIn("operation_descriptors_v1", response["protocol"]["features"])
        self.assertIsNone(ordinary._resources)
        ext, *_ = self.example()
        with self.assertRaises(RpcError):
            self.initialize(ext, features=False)
        self.assertFalse(ext.initialized)
        for invalid in [{"max_records": 256.0, "max_registrations_per_parent": 32},
                        {"max_records": 512, "max_registrations_per_parent": 32}]:
            with self.assertRaises(RpcError):
                self.initialize(ext, limits=invalid)
            self.assertFalse(ext.initialized)
        with self.assertRaises(ValueError):
            Extension(api_version="0.2").resource_type("example.Counter.v1")

    def test_unsupported_resource_shapes_and_duplicate_operations_fail_registration(self):
        ext, Counter, Input, Output, Use = self.example()
        initial = set(ext._tools)
        async def async_dispose(native):
            pass
        with self.assertRaises(TypeError):
            ext.resource_type("example.Async.v1", dispose=async_dispose)(type("Other", (), {}))
        self.assertEqual(len(ext._resources.types), 1)
        for annotation in [Optional[Resource[Counter]], list[Resource[Counter]], Resource[Counter]]:
            def handler(args: Input):
                return None
            handler.__annotations__["return"] = annotation
            with self.subTest(annotation=annotation), self.assertRaises(TypeError):
                ext.typed_tool(name="bad", description="Unsupported", summary=lambda value: "No.")(handler)
            self.assertEqual(set(ext._tools), initial)
        def handler(args: Use) -> int:
            return 1
        for options in [{"receiver": "/missing"}, {"operation_id": "create"}]:
            with self.assertRaises((TypeError, ValueError)):
                ext.typed_tool(name="bad", description="Unsupported", summary=lambda value: "No.", **options)(handler)
        self.assertEqual(set(ext._tools), initial)

    def test_failed_registration_does_not_claim_native_ownership(self):
        disposed = []
        ext, Counter, *_ = self.example(dispose=disposed.append)
        self.initialize(ext)
        def refuse(*args, **kwargs):
            raise RpcError(-32000, "quota_exceeded")
        ext.request = refuse
        with self.assertRaises(RpcError):
            ext.export(Counter())
        self.assertEqual(disposed, [])
        self.assertEqual(ext._resources.records, {})
        self.assertEqual(ext._resources.identities, set())


if __name__ == "__main__":
    unittest.main()
