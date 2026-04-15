export type SessionName = "claude" | "codex";
export type CopySurface = SessionName | "system" | null;

export type CopySelectionResult =
  | {
      kind: "dom";
      text: string;
    }
  | {
      kind: "session";
      session: SessionName;
      text: string;
    };

export interface ResolveCopySelectionInput {
  domSelection: string | null;
  lastActiveSession: SessionName | null;
  terminalSelections: Record<SessionName, string | null>;
}

export interface SystemSelectionGuardInput {
  systemSelection: string | null;
  activeSurface: CopySurface;
  resolvedSelection: CopySelectionResult | null;
}

export function resolveCopySelection(
  input: ResolveCopySelectionInput,
): CopySelectionResult | null {
  const domSelection = normalizeSelectionText(input.domSelection);
  if (domSelection) {
    return {
      kind: "dom",
      text: domSelection,
    };
  }

  if (!input.lastActiveSession) {
    return null;
  }

  const terminalSelection = normalizeSelectionText(
    input.terminalSelections[input.lastActiveSession],
  );
  if (!terminalSelection) {
    return null;
  }

  return {
    kind: "session",
    session: input.lastActiveSession,
    text: terminalSelection,
  };
}

export function shouldWarnUnsupportedSystemSelection(
  input: SystemSelectionGuardInput,
): boolean {
  const systemSelection = normalizeSelectionText(input.systemSelection);
  return Boolean(systemSelection && input.activeSurface === "system" && !input.resolvedSelection);
}

function normalizeSelectionText(text: string | null | undefined): string | null {
  if (typeof text !== "string") {
    return null;
  }

  return text.length > 0 ? text : null;
}
