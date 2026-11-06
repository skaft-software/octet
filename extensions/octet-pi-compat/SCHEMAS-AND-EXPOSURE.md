# Tool schemas and exposure

The Pi adapter retains JSON Schema constraints verbatim. Input and structured-output
validation share the native bounded validator, rather than maintaining different
keyword allowlists. The keyword profile covers the JSON Schema validation,
applicator, reference and annotation families used by Pi 1.0.2's TypeBox 1.3.27:
formats, pattern properties, numeric multiples, tuples/prefix items, contains,
conditionals, dependencies, boolean subschemas, references and unevaluated values.
Unknown default-format names are annotations, matching TypeBox's default registry.
References resolve within the supplied schema; validation never fetches remote
schemas, files or network resources.

Admission and evaluation retain independent 4096-work budgets, depth 32, bounded
constant/error sizes and bounded regular-expression compilation/backtracking.
Exhausting a validation budget is fatal, not a successful match or a recoverable
schema mismatch. Ordinary mismatches remain paired tool errors. Arbitrary
extension format-registry mutations are not imported into the native process.
Provider serializers retain the original non-strict schema; strict constrained
sampling continues to refuse unsupported structural keywords, with `prefer`
falling back to the unchanged non-strict schema.

Exposure follows Pi 1.0.2:

| Exposure | Active on registration* | Callable through another tool |
| --- | --- | --- |
| `direct` (default) | Yes | Only when active |
| `model-only` | Yes | No |
| `codemode` | No | Yes |
| `deferred` | No | Yes |
| `hidden` | No | No |

`defaultActive: false` disables automatic activation. Explicit activation can
declare codemode/deferred tools; hidden names are ignored. Registered-tool
snapshots report exposure even for inactive/hidden entries. Model and callable
catalogs freeze at one native registry revision for a model request. Exposure is
presentation/reachability, never a product-policy, effect, approval, owner,
generation, cancellation or accounting grant. Every nested execution still uses
the native broker; raw Pi helper execution has its separate reviewed route.

Focused checks: `schema-keywords.test.mjs`, `tool-exposure.test.mjs`, native
`json_repair::keyword_tests` and `extension::exposure_tests`. Package journey
qualification remains separate from these regression tests.
