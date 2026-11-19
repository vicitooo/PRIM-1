import { describe, expect, it } from "vitest";
import { RoomDraftStore } from "./room-drafts";

const live = (...ids: string[]) => new Set(ids);

describe("room-owned composer drafts", () => {
  it("restores each room's own draft across A -> B -> A", () => {
    const drafts = new RoomDraftStore();
    expect(drafts.bind("A", "", live("A", "B"))).toBe("");
    expect(drafts.bind("B", "for A", live("A", "B"))).toBe("");
    expect(drafts.bind("A", "for B", live("A", "B"))).toBe("for A");
    expect(drafts.bind("B", "for A, edited", live("A", "B"))).toBe("for B");
    expect(drafts.bind("A", "for B", live("A", "B"))).toBe("for A, edited");
  });

  it("leaves text, selection and focus alone for same-room renders", () => {
    const drafts = new RoomDraftStore();
    drafts.bind("A", "", live("A"));
    expect(drafts.bind("A", "typing", live("A"))).toBeNull();
    expect(drafts.ownerRoomId).toBe("A");
  });

  it("never carries a deleted room's draft into another room", () => {
    const drafts = new RoomDraftStore();
    drafts.bind("A", "", live("A", "B"));
    // A is deleted while its text is in the textarea; the inventory now lists only B.
    drafts.retire(live("B"));
    expect(drafts.bind("B", "A's unsent text", live("B"))).toBe("");
    // Nothing of A's survives if a room with a new id appears later.
    expect(drafts.bind("C", "", live("B", "C"))).toBe("");
    expect(drafts.bind("B", "", live("B", "C"))).toBe("");
  });

  it("retires stored drafts of deleted rooms only", () => {
    const drafts = new RoomDraftStore();
    drafts.bind("A", "", live("A", "B", "C"));
    drafts.bind("B", "draft A", live("A", "B", "C"));
    drafts.bind("C", "draft B", live("A", "B", "C"));
    drafts.retire(live("B", "C"));
    expect(drafts.bind("A", "", live("A", "B", "C"))).toBe("");
    expect(drafts.bind("B", "", live("A", "B", "C"))).toBe("draft B");
  });

  it("an empty textarea removes the room's stored draft", () => {
    const drafts = new RoomDraftStore();
    drafts.bind("A", "", live("A", "B"));
    drafts.bind("B", "x", live("A", "B"));
    drafts.bind("A", "", live("A", "B"));
    drafts.bind("B", "", live("A", "B"));
    expect(drafts.bind("A", "", live("A", "B"))).toBe("");
  });

  it("an empty inventory clears the textarea", () => {
    const drafts = new RoomDraftStore();
    drafts.bind("A", "", live("A"));
    expect(drafts.bind(null, "A's text", live())).toBe("");
  });

  it("submission completion clears only the submitting room's unchanged draft", () => {
    const drafts = new RoomDraftStore();
    drafts.bind("A", "", live("A", "B"));
    // Still in A, unchanged: clear the textarea.
    expect(drafts.completeSubmission("A", "hello", "hello")).toBe(true);
    // Still in A, but edited since submitting: keep the new text.
    expect(drafts.completeSubmission("A", "hello", "hello, more")).toBe(false);
    // Moved to B before A's Send finished: B's textarea is untouched, A's stored copy is cleared.
    drafts.bind("B", "hello", live("A", "B"));
    expect(drafts.completeSubmission("A", "hello", "B's own text")).toBe(false);
    expect(drafts.bind("A", "B's own text", live("A", "B"))).toBe("");
    expect(drafts.bind("B", "", live("A", "B"))).toBe("B's own text");
  });

  it("reports unsent text in the textarea or kept for another room (A108)", () => {
    const drafts = new RoomDraftStore();
    drafts.bind("A", "", live("A", "B"));
    expect(drafts.hasUnsentText("")).toBe(false);
    expect(drafts.hasUnsentText("typing")).toBe(true);
    drafts.bind("B", "for A", live("A", "B"));
    expect(drafts.hasUnsentText("")).toBe(true);
    // Back in A, its kept copy is shown; once cleared there, nothing is unsent.
    expect(drafts.bind("A", "", live("A", "B"))).toBe("for A");
    expect(drafts.hasUnsentText("")).toBe(false);
  });

  it("a stored draft edited after submitting is kept", () => {
    const drafts = new RoomDraftStore();
    drafts.bind("A", "", live("A", "B"));
    drafts.bind("B", "sent text plus edit", live("A", "B"));
    expect(drafts.completeSubmission("A", "sent text", "")).toBe(false);
    expect(drafts.bind("A", "", live("A", "B"))).toBe("sent text plus edit");
  });
});
