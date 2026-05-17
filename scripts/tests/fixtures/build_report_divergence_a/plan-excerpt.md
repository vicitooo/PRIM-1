# Scenario A divergence plan excerpt (synthetic fixture)

Mirrors a BUILD-report divergence: plan locked 50 integration
tests across 8 spec files; tree actually had 29 tests across 6 files.
Used as a regression fixture for the verifier.

## verifier: sub-scope A

- integration_tests: 50 across 8 files matching tree/integration/*.spec.ts
- spec_files: ["companies", "contacts", "tasks", "documents", "users", "teams", "segments", "interactions"]
- gate_scripts: []
