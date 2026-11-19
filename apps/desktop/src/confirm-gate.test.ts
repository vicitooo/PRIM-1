import { describe, expect, it, vi } from "vitest";
import { afterConfirm, closeSessionConfirmText, deleteRoomConfirmText, resumeHereConfirmText } from "./confirm-gate";

/** The renderer's command(): what Delete room and Close session would send. */
function commands() {
  return vi.fn(async (_name: string, _payload?: unknown) => undefined);
}

describe("afterConfirm (Delete room, Close session, Start fresh)", () => {
  // The dialog plugin's window.confirm returns a Promise: truthy, but not true.
  const answers: [string, () => Promise<boolean>][] = [
    ["no", async () => false],
    ["a failed dialog", async () => { throw new Error("dialog.confirm not allowed"); }],
    ["a non-boolean truthy value", async () => Promise.resolve("yes") as unknown as boolean],
  ];

  it("Delete room sends no command unless the answer is exactly true", async () => {
    for (const [name, ask] of answers) {
      const command = commands();
      const asked = vi.fn((_message: string) => ask());
      expect(await afterConfirm(() => asked(deleteRoomConfirmText("Walk room")), async () => {
        await command("delete_room", { request: { room_id: "r1" } });
      }), name).toBe(false);
      expect(asked).toHaveBeenCalledWith('Delete room "Walk room"? Its stored room history will be erased. Sessions will remain available.');
      expect(command, name).not.toHaveBeenCalled();
    }
    const command = commands();
    expect(await afterConfirm(async () => true, async () => { await command("delete_room"); })).toBe(true);
    expect(command).toHaveBeenCalledTimes(1);
  });

  it("Close session stops, removes and deletes nothing unless the answer is exactly true", async () => {
    const text = closeSessionConfirmText("Claude", "1a2b3c4d", ["stops it", 'removes it from room "Walk room"']);
    expect(text).toBe('Close "Claude" (1a2b3c4d)? This stops it and removes it from room "Walk room". Its terminal scrollback will be discarded.');
    for (const [name, ask] of answers) {
      const command = commands();
      await afterConfirm(ask, async () => {
        await command("remove_room_member");
        await command("stop_session");
        await command("delete_session");
      });
      expect(command, name).not.toHaveBeenCalled();
    }
  });
});

it("warns about external terminals as well as other apps before releasing a discovered conversation hold", () => {
  const message = resumeHereConfirmText("Codex member");
  expect(message).toContain("Codex member's conversation");
  expect(message).toContain("Close this conversation in every other app or terminal first");
  expect(message).toContain("two apps typing into one conversation will mix it up");
});
