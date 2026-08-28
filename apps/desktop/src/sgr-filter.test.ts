import { describe, expect, it } from "vitest";

import { SgrColourFilter, stripSgrColours } from "./sgr-filter";

const ESC = "\x1b";

describe("stripSgrColours", () => {
  it("keeps non-colour attributes and reset", () => {
    expect(stripSgrColours("")).toBe("");
    expect(stripSgrColours("0")).toBe("0");
    expect(stripSgrColours("1")).toBe("1");
    expect(stripSgrColours("1;3;4")).toBe("1;3;4");
    expect(stripSgrColours("0;1;31")).toBe("0;1");
    expect(stripSgrColours("2;90")).toBe("2");
  });

  it("drops basic, bright and background colours", () => {
    expect(stripSgrColours("31")).toBeNull();
    expect(stripSgrColours("91;103")).toBeNull();
    expect(stripSgrColours("39;49")).toBeNull();
    expect(stripSgrColours("1;35;44")).toBe("1");
  });

  it("drops 256-colour and truecolour forms with their arguments", () => {
    expect(stripSgrColours("38;5;208")).toBeNull();
    expect(stripSgrColours("38;2;255;107;107")).toBeNull();
    expect(stripSgrColours("1;38;2;255;107;107;4")).toBe("1;4");
    expect(stripSgrColours("48;5;17;3")).toBe("3");
    expect(stripSgrColours("38:2::255:107:107;1")).toBe("1");
    expect(stripSgrColours("58;5;3;4")).toBe("4");
  });
});

describe("SgrColourFilter", () => {
  it("rewrites colour sequences inside a chunk and leaves text alone", () => {
    const filter = new SgrColourFilter();
    expect(filter.apply(`a${ESC}[31mred${ESC}[0m b${ESC}[1;32mbold${ESC}[m`)).toBe(
      `ared${ESC}[0m b${ESC}[1mbold${ESC}[m`,
    );
  });

  it("never emits a torn sequence across chunk boundaries", () => {
    const filter = new SgrColourFilter();
    expect(filter.apply(`x${ESC}`)).toBe("x");
    expect(filter.apply("[38;2;")).toBe("");
    expect(filter.apply(`255;107;107mtext${ESC}[0`)).toBe("text");
    expect(filter.apply("m")).toBe(`${ESC}[0m`);
    expect(filter.flush()).toBe("");
  });

  it("hands a carried partial back untouched on flush", () => {
    const filter = new SgrColourFilter();
    expect(filter.apply(`tail${ESC}[1;3`)).toBe("tail");
    expect(filter.flush()).toBe(`${ESC}[1;3`);
    expect(filter.apply("4m")).toBe("4m");
  });

  it("does not hold back an over-long or non-SGR escape", () => {
    const filter = new SgrColourFilter();
    const long = `${ESC}[${"1;".repeat(40)}`;
    expect(filter.apply(long)).toBe(long);
    expect(filter.apply(`${ESC}]0;title\x07`)).toBe(`${ESC}]0;title\x07`);
    expect(filter.apply(`${ESC}[2J${ESC}[H`)).toBe(`${ESC}[2J${ESC}[H`);
  });
});
