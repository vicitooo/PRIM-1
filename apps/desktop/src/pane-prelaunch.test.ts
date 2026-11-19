import { describe, expect, it } from "vitest";

import { prelaunchView, searchPrompt } from "./pane-prelaunch";
import type { SessionSnapshot } from "./types";

function session(overrides: Partial<SessionSnapshot> = {}): SessionSnapshot {
  return {
    session_id: "a4d53b2a-c842-4496-a00e-daf1b04b4be5",
    alias: "session-a4d53b2a",
    label: "CC",
    driver: "claude",
    permission_profile: "unsafe",
    launch_choices: { model: null, effort: null, brief: { mode: "room_default" } },
    recovery: null,
    recovery_history: [],
    lifecycle_state: "closed",
    working_dir: "D:\\work",
    generation: 0,
    run_id: null,
    run_event_sequence: 0,
    process_id: null,
    running: false,
    resume_available: false,
    was_running_at_shutdown: false,
    last_activity_at: null,
    last_error: null,
    last_error_kind: null,
    ...overrides,
  };
}

describe("prelaunchView", () => {
  it("projects Sleeping without a fresh-start action, while real errors and recovery holds still win", () => {
    const asleep = session({ hibernated: true, resume_available: true });
    const view = prelaunchView(asleep);
    expect(view.kind).toBe("sleeping");
    expect(view.body).toContain("Wake resumes this conversation");
    expect(view.body).toContain("Stop keeps it stopped");
    expect(view.canStartFresh).toBe(false);
    expect(prelaunchView({ ...asleep, last_error: "process exited with code 3" }).kind).toBe("exit");
    expect(prelaunchView({ ...asleep, lifecycle_state: "failed", last_error: "cannot resume" }).kind).toBe("error");
    expect(prelaunchView({ ...asleep, recovery: { original_id: "id", provenance: "hold", reason: "Held" } }).kind).toBe("recovery");
  });

  it("keeps the idle overlay when nothing has failed", () => {
    expect(prelaunchView(session())).toEqual({
      kind: "idle",
      title: "CC pane ready.",
      body: "Launch the session from the header to begin.",
      canSearch: false,
      canStartFresh: false,
    });
  });

  it("names a missing host binary and offers the bounded search", () => {
    const view = prelaunchView(
      session({
        lifecycle_state: "failed",
        last_error: "Can't find claude.exe on PATH.",
        last_error_kind: "executable_not_found",
      }),
    );
    expect(view).toEqual({
      kind: "error",
      title: "CC did not start",
      body: "Can't find claude.exe on PATH.",
      canSearch: true,
      canStartFresh: false,
    });
    expect(
      searchPrompt(
        session({
          last_error: "Can't find claude.exe on PATH.",
          last_error_kind: "executable_not_found",
        }),
      ),
    ).toBe("Look in the usual places for Claude Code?");
  });

  it("does not offer search for any other launch failure", () => {
    expect(
      prelaunchView(
        session({
          lifecycle_state: "failed",
          last_error: "working directory unavailable: path gone",
          last_error_kind: null,
        }),
      ).canSearch,
    ).toBe(false);
  });

  it("keeps a retired nonzero exit distinct from a failed launch without requiring live output history", () => {
    const outcome = "process exited with code 3 — last output: END";
    const retired = session({ generation: 1, run_id: null, lifecycle_state: "closed", last_error: outcome });
    expect(prelaunchView(retired)).toEqual({
      kind: "exit", title: "CC exited", body: outcome, canSearch: false, canStartFresh: false,
    });
    // A later preflight failure can retain the prior generation; it still
    // needs its actionable startup error instead of being hidden as an exit.
    const failed = prelaunchView({ ...retired, lifecycle_state: "failed", last_error: "working directory unavailable" });
    expect(failed.kind).toBe("error");
    expect(failed.title).toBe("CC did not start");
  });

  it("does not offer search after the usual-places walk already missed", () => {
    expect(
      prelaunchView(
        session({
          lifecycle_state: "failed",
          last_error: "Looked in the usual places and still can't find claude.exe.",
          last_error_kind: null,
        }),
      ),
    ).toEqual({
      kind: "error",
      title: "CC did not start",
      body: "Looked in the usual places and still can't find claude.exe.",
      canSearch: false,
      canStartFresh: false,
    });
  });

  it("shows a recovery hold as its own state, never as did-not-start", () => {
    const view = prelaunchView(
      session({
        driver: "codex",
        label: "Codex",
        recovery: {
          original_id: "019a-old",
          provenance: "harness_resume_refusal",
          reason: "The stored conversation could not be resumed.",
        },
      }),
    );
    expect(view.kind).toBe("recovery");
    expect(view.title).toBe("Codex can't resume its conversation");
    expect(view.title).not.toContain("did not start");
    expect(view.body).toContain("The stored conversation could not be resumed.");
    expect(view.body).toContain("Its earlier conversation ID 019a-old is kept.");
    expect(view.canStartFresh).toBe(true);
    expect(view.canSearch).toBe(false);
  });

  it("says nothing about a kept ID when the hold has none", () => {
    const view = prelaunchView(
      session({
        recovery: { original_id: null, provenance: "p", reason: "Held." },
      }),
    );
    expect(view.body).not.toContain("is kept");
  });

  it("keeps the recovery action when a retired run also has an exit error", () => {
    const view = prelaunchView(session({
      driver: "codex", label: "Codex", lifecycle_state: "closed", running: false,
      generation: 1, run_id: null, last_error: "process exited with code 1",
      recovery: { original_id: "019a-old", provenance: "harness_resume_refusal",
        reason: "The stored conversation could not be resumed." },
    }));
    expect(view.kind).toBe("recovery");
    expect(view.title).toBe("Codex can't resume its conversation");
    expect(view.body).toContain("019a-old");
    expect(view.canStartFresh).toBe(true);
    expect(view.canSearch).toBe(false);
  });

  it("lets a real launch failure win over a recovery hold", () => {
    const view = prelaunchView(
      session({
        lifecycle_state: "failed",
        last_error: "working directory unavailable",
        recovery: { original_id: null, provenance: "p", reason: "Held." },
      }),
    );
    expect(view.kind).toBe("error");
    expect(view.title).toBe("CC did not start");
    expect(view.body).toBe("working directory unavailable");
    expect(view.canStartFresh).toBe(false);
  });
});
