import { describe, expect, it } from "vitest";

import { InitialTerminalBanner } from "./terminal-banner";

describe("InitialTerminalBanner", () => {
  it("removes the overlay before forwarding the first exact output chunk", () => {
    const events: Array<[string, string?]> = [];
    const banner = new InitialTerminalBanner(
      () => events.push(["remove"]),
      (chunk) => events.push(["write", chunk]),
    );
    const first = "raw\r\n\u001b[31mλ\0";

    banner.forwardOutput(first);
    banner.forwardOutput("second");

    expect(events).toEqual([
      ["remove"],
      ["write", first],
      ["write", "second"],
    ]);
  });

  it("dismisses once when the running snapshot wins the race", () => {
    const events: string[] = [];
    const banner = new InitialTerminalBanner(
      () => events.push("remove"),
      (chunk) => events.push(`write:${chunk}`),
    );

    banner.observeRunning(false);
    banner.observeRunning(true);
    banner.observeRunning(true);
    banner.forwardOutput("child");

    expect(events).toEqual(["remove", "write:child"]);
  });
});

it("reviewer hides a recovery overlay re-shown between runs", () => {
  let overlayVisible = true;
  const banner = new InitialTerminalBanner(
    () => { overlayVisible = false; },
    () => {},
  );
  banner.observeRunning(true);
  expect(overlayVisible).toBe(false);
  // TerminalPane.renderPrelaunch reuses this DOM element for a later recovery.
  banner.observeRunning(false);
  overlayVisible = true;
  banner.observeRunning(true);
  expect(overlayVisible).toBe(false);
});

it("late output of a stopped run keeps the recovery overlay shown for it", () => {
  let overlayVisible = true;
  const banner = new InitialTerminalBanner(
    () => { overlayVisible = false; },
    () => {},
  );
  banner.observeRunning(true);
  banner.observeRunning(false);
  overlayVisible = true; // renderPrelaunch shows the recovery
  banner.forwardOutput("final tail of the stopped run");
  expect(overlayVisible).toBe(true);
  banner.observeRunning(true);
  expect(overlayVisible).toBe(false);
});
