import { describe, expect, it } from "vitest";

import { resolveCopySelection, shouldWarnUnsupportedSystemSelection } from "./copy-selection";

describe("resolveCopySelection", () => {
  it("prefers DOM selection over a live terminal selection", () => {
    const result = resolveCopySelection({
      domSelection: String.raw`\\.\pipe\cli-master-wrapper-27788`,
      lastActiveSession: "claude",
      terminalSelections: {
        claude: "Claude pane ready.",
        codex: null,
      },
    });

    expect(result).toEqual({
      kind: "dom",
      text: String.raw`\\.\pipe\cli-master-wrapper-27788`,
    });
  });

  it("uses the last active terminal selection when there is no DOM selection", () => {
    const result = resolveCopySelection({
      domSelection: null,
      lastActiveSession: "codex",
      terminalSelections: {
        claude: "Claude pane ready.",
        codex: "Codex pane ready.",
      },
    });

    expect(result).toEqual({
      kind: "session",
      session: "codex",
      text: "Codex pane ready.",
    });
  });

  it("does not fall back to another pane when the last active pane has no selection", () => {
    const result = resolveCopySelection({
      domSelection: null,
      lastActiveSession: "codex",
      terminalSelections: {
        claude: "Claude pane ready.",
        codex: null,
      },
    });

    expect(result).toBeNull();
  });

  it("returns null when nothing is selected", () => {
    const result = resolveCopySelection({
      domSelection: null,
      lastActiveSession: null,
      terminalSelections: {
        claude: null,
        codex: null,
      },
    });

    expect(result).toBeNull();
  });
});

describe("shouldWarnUnsupportedSystemSelection", () => {
  it("warns only when the system log is the active copy surface", () => {
    const result = shouldWarnUnsupportedSystemSelection({
      systemSelection: "[11:57:10 AM] UI attached to supervisor.",
      activeSurface: "system",
      resolvedSelection: null,
    });

    expect(result).toBe(true);
  });

  it("does not warn after the user clicks back into an agent pane", () => {
    const resolvedSelection = resolveCopySelection({
      domSelection: null,
      lastActiveSession: "codex",
      terminalSelections: {
        claude: null,
        codex: "Codex pane ready.",
      },
    });

    const result = shouldWarnUnsupportedSystemSelection({
      systemSelection: "[11:57:10 AM] UI attached to supervisor.",
      activeSurface: "codex",
      resolvedSelection,
    });

    expect(result).toBe(false);
  });
});
