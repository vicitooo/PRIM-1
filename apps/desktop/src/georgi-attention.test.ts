import { describe, expect, it } from "vitest";
import { openAsks, operatorRequest, stripOperatorMention } from "./georgi-attention";
import type { RoomFeedEvent } from "./types";

const ROOM = "room-1";
const A = "agent-a";
const B = "agent-b";
let seq = 0;

function message(sender: RoomFeedEvent["item"] extends infer I ? I extends { kind: "message"; sender: infer S } ? S : never : never,
  content: string, recipients: string[] = [], id = `m${++seq}`): RoomFeedEvent {
  return {
    schema_version: 1, room_id: ROOM, cursor: { epoch: "e", sequence: ++seq }, timestamp: `2026-11-12T08:${String(seq).padStart(2, "0")}:00Z`,
    item: { kind: "message", message_id: id, request_id: id, sender, content, recipient_ids: recipients, membership_revision: 1 },
  } as RoomFeedEvent;
}

function card(author: string, key: string, kind: "question" | "decision" | "result", state: "open" | "done", title: string): RoomFeedEvent {
  return {
    schema_version: 1, room_id: ROOM, cursor: { epoch: "e", sequence: ++seq }, timestamp: "2026-11-12T09:00:00Z",
    item: { kind: "card", request_id: `c${seq}`, card_id: `c${seq}`, author,
      card: { key, kind, title, summary: "", body: "", state, data: {} } },
  } as RoomFeedEvent;
}

const label = (id: string) => (id === A ? "Claude First" : "Codex First");
const agent = (id: string) => ({ kind: "session" as const, session_id: id });
const you = { kind: "operator" as const };

describe("operator mentions", () => {
  it("reads @Georgi at the start, after a reply quote, bolded or alone on its line", () => {
    expect(operatorRequest("@Georgi: approve the deploy of #132?")).toBe("approve the deploy of #132?");
    expect(operatorRequest("> Operator: check speed\n\n**@Georgi** — need the API token")).toBe("need the API token");
    expect(operatorRequest("@georgi\n\n**Send this to Sam**\nbody")).toBe("Send this to Sam");
    expect(operatorRequest("Codex: @Georgi said yes")).toBeNull();
    expect(operatorRequest("Status update for the team")).toBeNull();
  });

  it("removes the mention for display but keeps the reply quote", () => {
    expect(stripOperatorMention("> Operator: hi\n\n@Georgi: please approve")).toBe("> Operator: hi\n\nplease approve");
  });
});

describe("open asks", () => {
  it("opens on @Georgi and closes only when you send to that agent", () => {
    const events = [
      message(agent(A), "@Georgi: approve deploy?"),
      message(agent(B), "@Georgi need a token"),
      message(agent(A), "Codex, take the next task", [B]),
    ];
    expect(openAsks(ROOM, events, label, new Set()).map((a) => [a.sender, a.text])).toEqual([
      ["Claude First", "approve deploy?"],
      ["Codex First", "need a token"],
    ]);
    const replied = [...events, message(you, "yes, deploy", [A])];
    expect(openAsks(ROOM, replied, label, new Set()).map((a) => a.sender)).toEqual(["Codex First"]);
    const posted = [...replied, message(you, "out for 30 min")];
    expect(openAsks(ROOM, posted, label, new Set()).map((a) => a.sender)).toEqual(["Codex First"]);
    const toAll = [...posted, message(you, "token is in the vault", [A, B])];
    expect(openAsks(ROOM, toAll, label, new Set())).toEqual([]);
  });

  it("counts open question and decision cards until done, and honours Done", () => {
    const events = [
      card(A, "deploy", "decision", "open", "Ship #132 today?"),
      card(A, "speed", "result", "open", "Speed measured"),
      card(B, "token", "question", "open", "Which account?"),
    ];
    const asks = openAsks(ROOM, events, label, new Set());
    expect(asks.map((a) => [a.kind, a.text])).toEqual([["decision", "Ship #132 today?"], ["question", "Which account?"]]);
    expect(openAsks(ROOM, [...events, card(A, "deploy", "decision", "done", "Ship #132 today?")], label, new Set()).map((a) => a.kind))
      .toEqual(["question"]);
    expect(openAsks(ROOM, events, label, new Set([`card:${B}:token`])).map((a) => a.kind)).toEqual(["decision"]);
  });
});
