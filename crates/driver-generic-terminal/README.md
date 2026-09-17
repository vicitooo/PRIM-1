# crates/driver-generic-terminal

Generic shell driver.

Responsibilities:

- construct a direct launch for the backend-qualified shell executable
- apply the session's working directory
- enforce the Normal-only permission profile and reject script shims

The supervisor owns lifecycle and room delivery. It can detect a supported
harness started inside the shell and adapt framing, but that does not add the
typed harness driver's complete work-state gating.
