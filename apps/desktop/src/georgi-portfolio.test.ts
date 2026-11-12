import { describe, expect, it } from "vitest";
import {
  addPortfolio, buildTree, loadGrouping, moveProject, placeBefore, placeRoom, portfolioNames, projectSummary, renamePortfolio, renameProject,
  roomNameInProject, seedGrouping, type Grouping,
} from "./georgi-portfolio";
import type { GeorgiMember } from "./georgi-ui";

const member = (id: string, status: GeorgiMember["status"]): GeorgiMember => ({ id, label: id, driver: "claude", status, reason: "" });

describe("portfolio grouping", () => {
  it("seeds known rooms by label and leaves the rest ungrouped", () => {
    const grouping: Grouping = { ...loadGrouping(), rooms: {}, loose: [] };
    const rooms = [
      { id: "1", label: "Notes Supervisor" },
      { id: "2", label: "Seaside-Backend" },
      { id: "3", label: "Seaside-Book" },
      { id: "4", label: "Checkout refactor" },
    ];
    expect(seedGrouping(grouping, rooms)).toBe(true);
    expect(grouping.rooms["1"]).toEqual({ portfolio: "Apps", project: "Notes", type: "lead" });
    expect(grouping.rooms["4"]).toBeUndefined();
    placeRoom(grouping, "2", null);
    expect(seedGrouping(grouping, rooms)).toBe(false);
    expect(grouping.rooms["2"]).toBeUndefined();
  });

  it("creates portfolios, moves whole projects and renames", () => {
    const grouping: Grouping = { ...loadGrouping(), rooms: {
      a: { portfolio: "Apps", project: "ReelStudio", type: "dev" },
      b: { portfolio: "Apps", project: "ReelStudio", type: "seo" },
      c: { portfolio: "Apps", project: "Notes", type: "lead" },
    }, portfolios: [], pinned: ["project:Apps/ReelStudio"], collapsed: ["Apps/ReelStudio"] };
    addPortfolio(grouping, "Clients");
    expect(portfolioNames(grouping)).toEqual(["Apps", "Clients"]);
    expect(buildTree([], grouping, new Map()).portfolios.map((p) => p.name)).toEqual(["Clients"]);
    moveProject(grouping, "Apps", "ReelStudio", "Clients");
    expect([grouping.rooms.a.portfolio, grouping.rooms.b.portfolio, grouping.rooms.c.portfolio]).toEqual(["Clients", "Clients", "Apps"]);
    expect(grouping.pinned).toEqual(["project:Clients/ReelStudio"]);
    expect(grouping.portfolios).toEqual([]);
    renamePortfolio(grouping, "Clients", "Partners");
    renameProject(grouping, "Partners", "ReelStudio", "Reel");
    expect(grouping.rooms.a).toEqual({ portfolio: "Partners", project: "Reel", type: "dev" });
    expect(grouping.pinned).toEqual(["project:Partners/Reel"]);
    expect(grouping.collapsed).toEqual(["Partners/Reel"]);
    moveProject(grouping, "Apps", "Notes", "Partners");
    expect(portfolioNames(grouping)).toEqual(["Apps", "Partners"]);
  });

  it("keeps your drag order instead of latest activity", () => {
    const grouping: Grouping = { ...loadGrouping(), pinned: [], portfolios: [], order: [], sort: "recent", rooms: {
      a: { portfolio: "Films", project: "ReelStudio", type: "dev" },
      b: { portfolio: "Films", project: "Funnels", type: "dev" },
      c: { portfolio: "Films", project: "SpiceRack", type: "dev" },
    } };
    const rooms = ["a", "b", "c"].map((id) => ({ id, label: id, members: [] }));
    const activity = new Map([["a", "2026-11-12T09:00:00Z"], ["b", "2026-11-12T08:00:00Z"], ["c", "2026-11-12T07:00:00Z"]]);
    const names = () => buildTree(rooms, grouping, new Map(), activity).portfolios[0].projects.map((p) => p.name);
    expect(names()).toEqual(["ReelStudio", "Funnels", "SpiceRack"]);
    placeBefore(grouping, names().map((n) => `project:Films/${n}`), "project:Films/SpiceRack", "project:Films/ReelStudio");
    expect(grouping.sort).toBe("manual");
    expect(names()).toEqual(["SpiceRack", "ReelStudio", "Funnels"]);
    grouping.sort = "recent";
    expect(names()).toEqual(["ReelStudio", "Funnels", "SpiceRack"]);
  });

  it("names rooms inside their project", () => {
    expect(roomNameInProject("Seaside-Backend", "Seaside", "dev")).toBe("Backend");
    expect(roomNameInProject("ReelStudio", "ReelStudio", "dev")).toBe("Development");
    expect(roomNameInProject("Notes Supervisor", "Notes", "lead")).toBe("Supervisor");
    expect(roomNameInProject("Checkout", "ReelStudio", "seo")).toBe("Checkout");
  });

  it("builds portfolio → project → rooms with lead first and rolled-up needs", () => {
    const grouping = { ...loadGrouping(), rooms: {
      a: { portfolio: "Apps", project: "ReelStudio", type: "seo" as const },
      b: { portfolio: "Apps", project: "ReelStudio", type: "dev" as const },
      c: { portfolio: "Apps", project: "Notes", type: "lead" as const },
    } };
    const tree = buildTree([
      { id: "a", label: "ReelStudio SEO", members: [member("x", "stopped")] },
      { id: "b", label: "ReelStudio", members: [member("y", "busy"), member("z", "needs")] },
      { id: "c", label: "Notes Supervisor", members: [] },
      { id: "d", label: "Loose room", members: [] },
    ], grouping, new Map([["a", 2]]));
    expect(tree.portfolios.map((p) => p.name)).toEqual(["Apps"]);
    expect(tree.portfolios[0].projects.map((p) => p.name)).toEqual(["Notes", "ReelStudio"]);
    const reel = tree.portfolios[0].projects[1];
    expect(reel.rooms.map((r) => r.name)).toEqual(["Development", "SEO"]);
    // Like a chat: the latest activity goes first; pinned stays on top.
    const active = buildTree([
      { id: "a", label: "ReelStudio SEO", members: [] },
      { id: "b", label: "ReelStudio", members: [] },
      { id: "c", label: "Notes Supervisor", members: [] },
    ], grouping, new Map(), new Map([["a", "2026-11-12T09:00:00Z"], ["b", "2026-11-12T08:00:00Z"], ["c", "2026-11-12T07:00:00Z"]]));
    expect(active.portfolios[0].projects.map((p) => p.name)).toEqual(["ReelStudio", "Notes"]);
    expect(active.portfolios[0].projects[0].rooms.map((r) => r.name)).toEqual(["SEO", "Development"]);
    const pinned = buildTree([
      { id: "a", label: "ReelStudio SEO", members: [] },
      { id: "c", label: "Notes Supervisor", members: [] },
    ], { ...grouping, pinned: ["project:Apps/Notes"] }, new Map(), new Map([["a", "2026-11-12T09:00:00Z"]]));
    expect(pinned.portfolios[0].projects.map((p) => p.name)).toEqual(["Notes", "ReelStudio"]);
    expect(reel.needs).toBe(3);
    expect(projectSummary(reel)).toBe("3 agents · 1 working");
    expect(tree.ungrouped.map((r) => r.id)).toEqual(["d"]);
  });
});
