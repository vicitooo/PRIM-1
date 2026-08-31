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
  it("keeps the idle overlay when nothing has failed", () => {
    expect(prelaunchView(session())).toEqual({
      kind: "idle",
      title: "CC pane ready.",
      body: "Launch the session from the header to begin.",
      canSearch: false,
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
    });
  });
});
