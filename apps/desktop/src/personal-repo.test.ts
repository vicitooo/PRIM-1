import { describe, expect, it } from "vitest";
import { personalRepoLocked, personalRepoStateText, type PersonalRepoStatus } from "./personal-repo";

const status = (patch: Partial<PersonalRepoStatus>): PersonalRepoStatus => ({
  folder: "D:\\All Repos\\victor",
  source: "default",
  status: "on",
  ...patch,
});

describe("personal repository state", () => {
  it("names where an open register comes from", () => {
    expect(personalRepoStateText(status({}))).toBe("On. This is the default: the workspace.");
    expect(personalRepoStateText(status({ source: "setting" }))).toBe("On. Chosen in Settings.");
    expect(personalRepoStateText(status({ source: "environment" }))).toContain("PRIM1_PERSONAL_REPO");
  });

  it("adds the writer's report to an open register", () => {
    const text = personalRepoStateText(status({ message: "Git was not found, so the session register files are written but not committed." }));
    expect(text).toBe("On. This is the default: the workspace. Git was not found, so the session register files are written but not committed.");
  });

  it("says why there is no register, in the backend's words", () => {
    const off = status({ folder: null, source: "environment", status: "off", message: "PRIM1_PERSONAL_REPO is off: this PRIM-1 keeps no session register." });
    expect(personalRepoStateText(off)).toBe(off.message);
    expect(personalRepoStateText(status({ status: "error", message: "The folder D:\\x does not exist." }))).toBe("The folder D:\\x does not exist.");
    expect(personalRepoStateText(status({ status: "off" }))).toContain("no session register");
    expect(personalRepoStateText(status({ status: "error" }))).toContain("cannot be used");
  });

  it("locks the Settings controls only while the environment decides", () => {
    expect(personalRepoLocked(status({ source: "environment" }))).toBe(true);
    expect(personalRepoLocked(status({ source: "setting" }))).toBe(false);
    expect(personalRepoLocked(status({}))).toBe(false);
  });
});
