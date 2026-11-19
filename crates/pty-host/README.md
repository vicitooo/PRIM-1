# crates/pty-host

PTY ownership and transport.

Responsibilities:

- create PTYs
- spawn child processes into PTYs
- read terminal output
- inject terminal input
- resize handling
- close/cleanup

This crate should stay focused on terminal/process mechanics, not supervisor policy.

On Windows, natural process exit does not itself release the ConPTY producer.
`finish_output_if_scope_exited` closes its retained master only after the exact
child and the complete retained Job have exited. Run it away from the reader and
supervisor locks: `ClosePseudoConsole` can emit a final frame and wait for output
drain on older Windows. The independent reader stays open through its decoder
tail and real EOF. Live descendants defer closure; this operation never kills
them, cancels output, invents EOF or clears parser credits. Ending descendants
an exited hosted agent left behind is the supervisor's drain grace, through the
ordinary `kill`, never this operation. See the
natural-drain handoff (a development note that is not part of this repository).
