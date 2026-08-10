# crates/driver-prime

Prime Agent driver for the measured Ubuntu WSL boundary.

Responsibilities:

- construct a shell-free `wsl.exe` launch with absolute Linux executables
- bind each run to one exact user-systemd transient service
- run an immutable guard that revalidates the selected Linux cwd identity
  immediately before spawning `prime-agent`
- expose only the `Normal` permission profile

The supervisor owns cwd qualification, stale-service reconciliation, start-time
service confirmation, dual Linux/Windows termination proof, and the decision to
withhold pane sideband and synthetic routed delivery. This crate does not own
general lifecycle policy.
