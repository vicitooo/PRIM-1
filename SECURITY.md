# Reporting security vulnerabilities

Email **vic.valtchev@gmail.com** with the subject **PRIM-1 security report**. Please report suspected vulnerabilities privately before opening a public issue.

Include the affected PRIM-1 commit, Windows and CLI versions, reproduction steps, and the impact you observed. Use a minimal example and remove credentials, personal data, and private conversation content. Do not test against systems or accounts you do not control.

PRIM-1 is an alpha. Its permission profiles and pane protocol are described in [README-AI.md](README-AI.md) and [RUNTIME-CONTRACTS.md](RUNTIME-CONTRACTS.md); it is not a sandbox for hostile programs running as the same operating-system user.

An outside-agent invitation token ([docs/GUEST-AGENTS.md](docs/GUEST-AGENTS.md)) lets its holder read one room and post to it, which types text into that room's agents. It sits in plain text in the outside agent's MCP configuration; keep that configuration out of the files PRIM-1's own panes load (a token presented from inside a pane is refused), and revoke it when that agent is done. Guest messages are outside input to the members, never the operator's instructions: they arrive with every line quoted, so a guest cannot type a line that passes for a PRIM-1 header.

General bugs and feature requests belong in [Issues](https://github.com/vicitooo/PRIM-1/issues).
