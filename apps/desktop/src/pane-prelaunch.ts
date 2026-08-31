import { driverLabel } from "./session-tabs";
import type { SessionSnapshot } from "./types";

export type PrelaunchView =
  | {
      kind: "idle";
      title: string;
      body: string;
      canSearch: false;
    }
  | {
      kind: "error";
      title: string;
      body: string;
      canSearch: boolean;
    };

export function prelaunchView(session: SessionSnapshot): PrelaunchView {
  if (session.last_error) {
    return {
      kind: "error",
      title: `${session.label} did not start`,
      body: session.last_error,
      canSearch: session.last_error_kind === "executable_not_found",
    };
  }
  return {
    kind: "idle",
    title: `${session.label} pane ready.`,
    body: "Launch the session from the header to begin.",
    canSearch: false,
  };
}

export function searchPrompt(session: SessionSnapshot): string {
  return `Look in the usual places for ${driverLabel(session.driver)}?`;
}
