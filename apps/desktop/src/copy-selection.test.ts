import { describe, expect, it } from "vitest";

import { resolveCopySelection } from "./copy-selection";

describe("resolveCopySelection", () => {
  it("prefers DOM selection over terminal selections", () => {
    const result = resolveCopySelection({
      domSelection: String.raw`\\.\pipe\cli-master-wrapper-27788`,
      activeSurface: "claude",
      terminalSelections: {
        claude: "Claude pane ready.",
        codex: null,
      },
      systemSelection: "[11:57:10 AM] UI attached to supervisor.",
    });

    expect(result).toEqual({
      kind: "dom",
      text: String.raw`\\.\pipe\cli-master-wrapper-27788`,
    });
  });

  it("uses the active agent pane selection when there is no DOM selection", () => {
    const result = resolveCopySelection({
      domSelection: null,
      activeSurface: "codex",
      terminalSelections: {
        claude: "Claude pane ready.",
        codex: "Codex pane ready.",
      },
      systemSelection: "[11:57:10 AM] UI attached to supervisor.",
    });

    expect(result).toEqual({
      kind: "session",
      session: "codex",
      text: "Codex pane ready.",
    });
  });

  it("copies the system log when the system surface is active", () => {
    const result = resolveCopySelection({
      domSelection: null,
      activeSurface: "system",
      terminalSelections: {
        claude: "Claude pane ready.",
        codex: "Codex pane ready.",
      },
      systemSelection: "[11:57:10 AM] UI attached to supervisor.",
    });

    expect(result).toEqual({
      kind: "system",
      text: "[11:57:10 AM] UI attached to supervisor.",
    });
  });

  it("does not fall back to another surface when the active pane has no selection", () => {
    const result = resolveCopySelection({
      domSelection: null,
      activeSurface: "codex",
      terminalSelections: {
        claude: "Claude pane ready.",
        codex: null,
      },
      systemSelection: "[11:57:10 AM] UI attached to supervisor.",
    });

    expect(result).toBeNull();
  });

  it("does not fall back to the system log after the user clicks back into an agent pane", () => {
    const result = resolveCopySelection({
      domSelection: null,
      activeSurface: "codex",
      terminalSelections: {
        claude: null,
        codex: "Codex pane ready.",
      },
      systemSelection: "[11:57:10 AM] UI attached to supervisor.",
    });

    expect(result).toEqual({
      kind: "session",
      session: "codex",
      text: "Codex pane ready.",
    });
  });

  it("returns null when nothing is selected", () => {
    const result = resolveCopySelection({
      domSelection: null,
      activeSurface: null,
      terminalSelections: {
        claude: null,
        codex: null,
      },
      systemSelection: null,
    });

    expect(result).toBeNull();
  });
});
