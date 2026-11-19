import { driverLabel } from "./session-tabs";
import { isSleeping } from "./hibernation";
import type { SessionSnapshot } from "./types";

export type PrelaunchView =
  | {
      kind: "sleeping";
      title: string;
      body: string;
      canSearch: false;
      canStartFresh: false;
    }
  | {
      kind: "idle";
      title: string;
      body: string;
      canSearch: false;
      canStartFresh: false;
    }
  | {
      kind: "error";
      title: string;
      body: string;
      canSearch: boolean;
      canStartFresh: false;
    }
  | {
      kind: "exit";
      title: string;
      body: string;
      canSearch: false;
      canStartFresh: false;
    }
  | {
      kind: "recovery";
      title: string;
      body: string;
      canSearch: false;
      canStartFresh: true;
    };

/** A closed run's last_error describes its exit, not failure to launch.
    Launch failures use failed; recovery holds are separate (S5). */
export function prelaunchView(session: SessionSnapshot): PrelaunchView {
  const retired = !session.running && session.lifecycle_state === "closed";
  // A retired run's error must not hide its existing recovery action.
  // Failed launch/preflight errors still take precedence over a stored hold.
  if (session.last_error && (!retired || !session.recovery)) {
    if (retired) {
      return {
        kind: "exit",
        title: `${session.label} exited`,
        body: session.last_error,
        canSearch: false,
        canStartFresh: false,
      };
    }
    return {
      kind: "error",
      title: `${session.label} did not start`,
      body: session.last_error,
      canSearch: session.last_error_kind === "executable_not_found",
      canStartFresh: false,
    };
  }
  if (session.recovery) {
    const kept = session.recovery.original_id
      ? ` Its earlier conversation ID ${session.recovery.original_id} is kept.`
      : "";
    return {
      kind: "recovery",
      title: `${session.label} can't resume its conversation`,
      body: `${session.recovery.reason}${kept} Start fresh session begins a new conversation; earlier references stay in its history.`,
      canSearch: false,
      canStartFresh: true,
    };
  }
  if (isSleeping(session)) {
    return {
      kind: "sleeping",
      title: `${session.label} is sleeping.`,
      body: "Wake resumes this conversation. New room messages can wake it; Stop keeps it stopped.",
      canSearch: false,
      canStartFresh: false,
    };
  }
  return {
    kind: "idle",
    title: `${session.label} pane ready.`,
    body: "Launch the session from the header to begin.",
    canSearch: false,
    canStartFresh: false,
  };
}

export function searchPrompt(session: SessionSnapshot): string {
  return `Look in the usual places for ${driverLabel(session.driver)}?`;
}
