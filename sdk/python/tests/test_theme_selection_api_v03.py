"""Behavioural tests for the API 0.3 host-mediated theme-selection capability.

These exercise the generated ``ThemeSelectParams``/``ThemeSelectResult`` wire
models and the generated ``resolve_theme_selection`` host policy.  The policy is
bounded and fail-closed: unknown namespaces, theme ids, and roles are rejected,
a selection can never widen project trust, and two extensions cannot shadow each
other's namespace.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from octet_extension.api_v03 import (  # noqa: E402
    MAX_THEME_ID_BYTES,
    THEME_ROLES,
    THEME_TRUST_VALUES,
    ContractError,
    ThemeSelectParams,
    ThemeSelectResult,
    resolve_theme_selection,
    validate_theme_select_params,
    validate_theme_select_result,
)


def params(**overrides: object) -> ThemeSelectParams:
    base = {
        "namespace": "ext.alpha",
        "theme_id": "solarized",
        "role": "accent",
        "scope": "extension",
    }
    base.update(overrides)
    return ThemeSelectParams(**base)  # type: ignore[arg-type]


class ThemeSelectionCapabilityTests(unittest.TestCase):
    def test_valid_selection_resolves_under_the_requesting_namespace(self) -> None:
        result = resolve_theme_selection(
            params(), "ext.alpha", {"solarized": "compiled", "octet-dark": "user"}
        )
        self.assertEqual(result.status, "selected")
        self.assertEqual(result.theme_id, "solarized")

    def test_unknown_namespace_is_rejected(self) -> None:
        # A different extension cannot select a theme under another's namespace.
        with self.assertRaises(ContractError) as caught:
            resolve_theme_selection(params(), "ext.beta", {"solarized": "compiled"})
        self.assertEqual(caught.exception.name, "capability_mismatch")

    def test_two_extensions_cannot_shadow_each_other(self) -> None:
        catalog = {"solarized": "compiled"}
        alpha = resolve_theme_selection(params(), "ext.alpha", catalog)
        beta = resolve_theme_selection(
            params(namespace="ext.beta"), "ext.beta", catalog
        )
        self.assertEqual(alpha.theme_id, beta.theme_id)
        # Alpha selecting cannot make a beta-namespaced request succeed.
        with self.assertRaises(ContractError):
            resolve_theme_selection(params(namespace="ext.beta"), "ext.alpha", catalog)

    def test_unknown_theme_id_is_rejected(self) -> None:
        with self.assertRaises(ContractError) as caught:
            resolve_theme_selection(params(theme_id="does-not-exist"), "ext.alpha", {})
        self.assertEqual(caught.exception.name, "invalid_params")
        self.assertEqual(caught.exception.code, -32602)

    def test_unknown_role_is_rejected(self) -> None:
        with self.assertRaises(ContractError) as caught:
            validate_theme_select_params(params(role="not-a-role"))
        self.assertEqual(caught.exception.name, "invalid_params")

    def test_every_known_role_is_accepted(self) -> None:
        for role in THEME_ROLES:
            validate_theme_select_params(params(role=role))

    def test_scope_cannot_be_widened(self) -> None:
        # Only the extension scope is representable, so a request can never
        # widen project trust through this capability.
        for scope in ("project", "global", "workspace"):
            with self.assertRaises(ContractError) as caught:
                validate_theme_select_params(params(scope=scope))
            self.assertEqual(caught.exception.name, "invalid_params")

    def test_widening_trust_is_rejected(self) -> None:
        with self.assertRaises(ContractError) as caught:
            resolve_theme_selection(
                params(), "ext.alpha", {"solarized": "project"}
            )
        self.assertEqual(caught.exception.name, "capability_mismatch")
        self.assertEqual(caught.exception.code, -32011)

    def test_unknown_trust_value_is_rejected(self) -> None:
        with self.assertRaises(ContractError):
            resolve_theme_selection(
                params(), "ext.alpha", {"solarized": "root"}
            )

    def test_overlong_theme_id_is_rejected(self) -> None:
        with self.assertRaises(ContractError) as caught:
            validate_theme_select_params(
                params(theme_id="t" * (MAX_THEME_ID_BYTES + 1))
            )
        self.assertEqual(caught.exception.name, "resource_exhausted")

    def test_unknown_fields_are_rejected(self) -> None:
        wire = params().to_wire()
        wire["persist"] = True
        with self.assertRaises(ContractError) as caught:
            ThemeSelectParams.from_wire(wire)
        self.assertEqual(caught.exception.name, "invalid_params")

    def test_result_status_is_bounded(self) -> None:
        with self.assertRaises(ContractError):
            validate_theme_select_result(
                ThemeSelectResult(status="escalated", theme_id=None, reason=None)
            )

    def test_capability_and_method_are_registered_as_optional(self) -> None:
        from octet_extension.api_v03 import CAPABILITY_SPECS, METHOD_SPECS

        capability = next(
            entry for entry in CAPABILITY_SPECS if entry[0] == "theme_selection"
        )
        self.assertFalse(capability[1])  # not required by default
        self.assertTrue(capability[2])  # available
        method = next(entry for entry in METHOD_SPECS if entry[0] == "theme/select")
        self.assertEqual(method[1], "extension_to_host")
        self.assertEqual(method[2], "theme_selection")
        self.assertFalse(method[3])  # not required by default
        self.assertTrue(method[4])  # available

    def test_trust_vocabulary_excludes_widening_scope(self) -> None:
        self.assertEqual(set(THEME_TRUST_VALUES), {"compiled", "user"})
        self.assertNotIn("project", THEME_TRUST_VALUES)


if __name__ == "__main__":
    unittest.main()
