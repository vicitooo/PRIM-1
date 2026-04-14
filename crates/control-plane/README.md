# crates/control-plane

Local control plane transport.

Expected responsibilities:

- named pipe server on Windows
- Unix socket server on POSIX
- sideband message ingress
- validation and normalization into shared runtime events

This crate is the reliable command/control path, separate from PTY-visible output.

