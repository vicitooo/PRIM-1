# CLI-master-wrapper — Project Structure

**Status:** Planned scaffold
**Date:** 2026-04-14

## Root

- `README.md`
  Project overview and scope

- `ARCHITECTURE.md`
  Canonical system design

- `IMPLEMENTATION-PLAN.md`
  Phase plan and milestones

- `STACK-DECISIONS.md`
  Locked choices and scope boundaries

- `RUNTIME-CONTRACTS.md`
  Contracts for messages, lifecycle, permissions, logging

- `PHASE-0-RESEARCH.md`
  Research checklist before implementation

- `TASK-BREAKDOWN.md`
  Work packages and dependencies

## App

- `apps/desktop/`
  Tauri desktop shell and xterm.js-based UI

## Crates

- `crates/shared-types/`
  Common Rust types for sessions, lifecycle, messages, audit events

- `crates/pty-host/`
  PTY backend ownership and process attachment

- `crates/control-plane/`
  Named pipe / Unix socket transport and routing glue

- `crates/supervisor/`
  Runtime brain: registry, lifecycle, routing, audit, usage telemetry

- `crates/driver-claude/`
  Claude-specific launch/resume/parse behavior

- `crates/driver-codex/`
  Codex-specific launch/resume/parse behavior

- `crates/driver-generic-terminal/`
  Fallback generic terminal app wrapper

## Support

- `scripts/`
  Human/operator and agent-callable scripts like send/spawn/restart helpers

- `tests/`
  Unit/integration harness and documented testing conventions

## Design principle

The folder structure should preserve the system boundaries:

- PTY ownership separate from supervisor policy
- control plane separate from UI
- driver logic separate from generic runtime
- shared contracts centralized

If a future file placement starts mixing those concerns, it is a warning sign.

