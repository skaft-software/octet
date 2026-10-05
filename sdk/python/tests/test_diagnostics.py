import copy
import io
import json
from pathlib import Path
import unittest

from octet_extension import (
    ArtifactSource, BlobSource, Diagnostic, DiagnosticAttachment, DiagnosticEdit,
    DiagnosticFix, DiagnosticLocation, DiagnosticRelated, DiagnosticSpan,
    Extension, RpcError, WorkspaceSource, diagnostic_summary, text_content,
    tool_result, validate_diagnostics,
)


class DiagnosticTests(unittest.TestCase):
    def diagnostic(self):
        location = DiagnosticLocation(WorkspaceSource("netlists/a.cir", "0" * 64), DiagnosticSpan(0, 2))
        return Diagnostic("warning", "solver.floating", "Floating node.\nAdd ground.", primary=location,
                          related=(DiagnosticRelated("Related.", location),),
                          fixes=(DiagnosticFix("Ground", (DiagnosticEdit(location, "ground"),)),),
                          attachments=(DiagnosticAttachment("artifact", "opaque-id", "Report"),))

    def test_A05_shared_profile(self):
        fixture = json.loads((Path(__file__).parents[2] / "conformance/typed-values-v1.json").read_text())
        for value in fixture["diagnostics"]["valid"]:
            validate_diagnostics([value])
        for value in fixture["diagnostics"]["invalid"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                validate_diagnostics([value])

    def test_A05_typed_locations_fixes_and_projection(self):
        diagnostic = self.diagnostic()
        wire = diagnostic.to_wire()
        self.assertEqual(wire["primary"]["span"], {"start_byte": 0, "end_byte": 2})
        self.assertEqual(wire["fixes"][0]["edits"][0]["replacement"], "ground")
        result = tool_result(text_content("Validation failed."), is_error=True, diagnostics=[diagnostic])
        self.assertEqual(result["metadata"]["octet_diagnostics_v1"], [wire])
        self.assertEqual(result["content"][1]["text"], "warning[solver.floating]: Floating node. Add ground.")
        ext = Extension(api_version="0.4", stderr=io.StringIO())
        @ext.tool(name="diagnose", description="Domain diagnostics")
        def diagnose(args):
            return result
        self.assertEqual(ext._call_tool({"name": "diagnose"})["content"], result["content"])
        bare = Diagnostic("info", "solver.ready", "Ready.").to_wire()
        self.assertNotIn("primary", bare)
        for source in [BlobSource("blob-id"), ArtifactSource("artifact-id")]:
            value = Diagnostic("info", "solver.ready", "Ready.", primary=DiagnosticLocation(source, DiagnosticSpan(0, 1)))
            validate_diagnostics([value.to_wire()])

    def test_raw_reserved_profile_cannot_bypass_validation(self):
        ext = Extension(api_version="0.4", stderr=io.StringIO())
        value = {"severity": "fatal", "code": "bad.severity", "message": "No."}
        @ext.tool(name="raw", description="Raw diagnostics")
        def raw(args):
            return tool_result(text_content("No."), metadata={"octet_diagnostics_v1": [value]})
        with self.assertRaises(RpcError) as raised:
            ext._call_tool({"name": "raw"})
        self.assertEqual(raised.exception.code, -32603)
        value["severity"] = "error"
        result = ext._call_tool({"name": "raw"})
        self.assertEqual(result["content"][1]["text"], "error[bad.severity]: No.")
        with self.assertRaises(ValueError):
            tool_result(text_content("No."), diagnostics=[self.diagnostic()], metadata={"octet_diagnostics_v1": []})

    def test_all_nested_records_are_closed_and_nonnullable(self):
        original = self.diagnostic().to_wire()
        for path in [(), ("primary",), ("primary", "source"), ("primary", "span"), ("related", 0),
                     ("fixes", 0), ("fixes", 0, "edits", 0), ("attachments", 0)]:
            value = copy.deepcopy(original)
            node = value
            for key in path:
                node = node[key]
            node["unexpected"] = True
            with self.subTest(path=path), self.assertRaises(ValueError):
                validate_diagnostics([value])
        for key in ("primary", "related", "fixes", "attachments"):
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate_diagnostics([{**original, key: None}])

    def test_limits_and_workspace_revision_checks(self):
        original = self.diagnostic().to_wire()
        for field, values in [("severity", [None, [], 1]), ("code", ["", "a" * 129, "1bad", "λ"]),
                              ("message", ["", "a" * 4097, "\x1b", "\x85", "\ud800"])]:
            for value in values:
                with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                    validate_diagnostics([{**original, field: value}])
        for path in ["/etc/passwd", "../x", "a/./b", "a//b", "a\\b", "C:x", "a\nb"]:
            value = copy.deepcopy(original)
            value["primary"]["source"]["path"] = path
            with self.subTest(path=path), self.assertRaises(ValueError):
                validate_diagnostics([value])
        for span in [{"start_byte": True, "end_byte": 2}, {"start_byte": 2, "end_byte": 1},
                     {"start_byte": 0, "end_byte": 2**53}]:
            value = copy.deepcopy(original)
            value["primary"]["span"] = span
            with self.assertRaises(ValueError):
                validate_diagnostics([value])
        value = copy.deepcopy(original)
        value["fixes"][0]["edits"] = []
        with self.assertRaises(ValueError):
            validate_diagnostics([value])
        value = copy.deepcopy(original)
        value["fixes"][0]["edits"][0]["location"]["source"] = {"kind": "blob", "id": "opaque"}
        with self.assertRaises(ValueError):
            validate_diagnostics([value])
        for key, maximum in [("related", 16), ("fixes", 8), ("attachments", 16)]:
            value = copy.deepcopy(original)
            value[key] *= maximum + 1
            with self.assertRaises(ValueError):
                validate_diagnostics([value])
        with self.assertRaises(ValueError):
            validate_diagnostics([original] * 33)
        large = {"severity": "info", "code": "test", "message": "x" * 4096}
        with self.assertRaises(ValueError):
            validate_diagnostics([large] * 16)

    def test_projection_bounds_are_utf8_safe(self):
        values = [{"severity": "info", "code": f"test.{i}", "message": "one\ntwo\tthree"} for i in range(9)]
        summary = diagnostic_summary(values)
        self.assertEqual(len(summary.splitlines()), 8)
        self.assertNotIn("test.8", summary)
        text = diagnostic_summary([{**values[0], "message": "😀" * 1024}])
        self.assertLessEqual(len(text.encode("utf-8")), 4096)
        self.assertEqual(diagnostic_summary([]), "")


if __name__ == "__main__":
    unittest.main()
