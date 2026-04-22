# Sub-scope A — BUILD_A_V1 plan excerpt (synthetic fixture)

Mirrors the real BUILD_A_V1 failure: plan locked 50 integration
tests across 8 spec files; tree actually had 29 tests across 6 files.
Used as a regression fixture for the verifier.

## verifier: sub-scope A

- integration_tests: 50 across 8 files matching tree/integration/*.spec.ts
- spec_files: ["companies", "contacts", "tasks", "documents", "users", "teams", "segments", "interactions"]
- gate_scripts: []
