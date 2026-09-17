# crates/control-plane

JSON codec and shared frame limit for the local control protocol.

Responsibilities:

- encode and decode typed requests and responses
- define the maximum frame size used by the transport
- reject malformed protocol input

The Windows named-pipe server and caller authorization live in
`crates/supervisor`. This crate does not implement a POSIX socket server.
