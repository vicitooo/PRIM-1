# crates/supervisor

Supervisor runtime.

Responsibilities:

- process registry
- session registry
- persistent session/room catalogs and conversation references
- lifecycle transitions
- routing
- room membership, feed, briefing, and recipient delivery
- named-pipe server and per-run caller authorization
- restart policy
- audit logging
- enforcement of working-root and capability policies

This crate is the authority of the system.
