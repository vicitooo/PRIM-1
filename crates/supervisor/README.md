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

Fresh liveness sweeps capture exact run/PTY ownership, perform native process
queries outside the shared shutdown/session locks, and revalidate before
applying results. Sweeps serialize independently; the retirement/shutdown
protocol is unchanged. Remaining lock and command-thread work is recorded in
the liveness-probes analysis (a development note that is not part of this repository).

Desktop output credit waits while valid current-epoch ACK/discard progress
continues, with a two-second idle limit and five-second hard wait ceiling
(initial policy, not measured latency targets). A renderer that stops acknowledging
output cannot indefinitely stop native PTY observation: the run continues in
observer mode, while the desktop consumes control strings and omits display
payload with a coalesced visible gap. Existing tickets retain their exact
epoch/run ownership and 64 KiB/64 item per-run, 1 MiB/256 item aggregate limits.
Reservations charge 64 extra bytes for escape carry and a bounded replay of
known renderer modes before display recovery. Real credit availability resumes display; omitted history
and screen positioning cannot be reconstructed. See
the natural-drain analysis (a development note that is not part of this repository)
for the natural ConPTY closure and bounded renderer-stall corrections.

The read-only `output_credit_metrics` desktop command reports fixed per-epoch
dispatch-to-parser-ACK latency counters/histogram, observer entry/current-run
counts, and current/peak reserved ledger usage. It keeps no output or per-run
history and emits no periodic events. These timings include WebView/parser/ACK
transport delay; they measure neither paint completion nor total PTY latency.
