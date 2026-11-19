/** Unsent room-composer text, owned per room and kept in memory only. The one
    composer textarea shows the active room's draft: switching rooms saves the
    outgoing room's text and restores the incoming room's, deleting a room
    retires its draft, and text never follows the operator into another room. */
export class RoomDraftStore {
  private readonly drafts = new Map<string, string>();
  private owner: string | null = null;

  /** The room whose draft the textarea currently shows. */
  get ownerRoomId(): string | null {
    return this.owner;
  }

  /** Move the textarea to `roomId`. Returns the text it should now show, or
      null when the room is unchanged (leave text, selection and focus alone).
      The outgoing text is kept only while its room still exists. */
  bind(roomId: string | null, currentText: string, liveRoomIds: ReadonlySet<string>): string | null {
    if (roomId === this.owner) {
      return null;
    }
    if (this.owner !== null && liveRoomIds.has(this.owner)) {
      if (currentText) {
        this.drafts.set(this.owner, currentText);
      } else {
        this.drafts.delete(this.owner);
      }
    }
    this.owner = roomId;
    return roomId === null ? "" : this.drafts.get(roomId) ?? "";
  }

  /** Any unsent room text: the textarea's, or one kept for another room
      (A108: an upgrade waits while there is). The shown room's stored copy
      is stale while the textarea is its truth. */
  hasUnsentText(currentText: string): boolean {
    return currentText !== "" || Array.from(this.drafts.keys()).some((roomId) => roomId !== this.owner);
  }

  /** Forget the drafts of rooms that no longer exist. */
  retire(liveRoomIds: ReadonlySet<string>): void {
    for (const roomId of Array.from(this.drafts.keys())) {
      if (!liveRoomIds.has(roomId)) {
        this.drafts.delete(roomId);
      }
    }
  }

  /** After a successful Post/Send from `roomId` of exactly `content`. Returns
      true when the textarea still shows that room's unchanged text and should
      be cleared; otherwise clears that room's stored draft only if unchanged.
      Another room's text is never touched. */
  completeSubmission(roomId: string, content: string, currentText: string): boolean {
    if (this.owner === roomId) {
      return currentText === content;
    }
    if (this.drafts.get(roomId) === content) {
      this.drafts.delete(roomId);
    }
    return false;
  }
}
