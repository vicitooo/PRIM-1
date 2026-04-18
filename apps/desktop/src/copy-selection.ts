export type SessionName = string;
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
    }
  | {
      kind: "system";
      text: string;
    };

export interface ResolveCopySelectionInput {
  domSelection: string | null;
  activeSurface: CopySurface;
  terminalSelections: Record<string, string | null>;
  systemSelection: string | null;
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

  if (input.activeSurface === "system") {
    const systemSelection = normalizeSelectionText(input.systemSelection);
    if (!systemSelection) {
      return null;
    }

    return {
      kind: "system",
      text: systemSelection,
    };
  }

  if (!input.activeSurface) {
    return null;
  }

  const terminalSelection = normalizeSelectionText(
    input.terminalSelections[input.activeSurface],
  );
  if (!terminalSelection) {
    return null;
  }

  return {
    kind: "session",
    session: input.activeSurface,
    text: terminalSelection,
  };
}

function normalizeSelectionText(text: string | null | undefined): string | null {
  if (typeof text !== "string") {
    return null;
  }

  return text.length > 0 ? text : null;
}
