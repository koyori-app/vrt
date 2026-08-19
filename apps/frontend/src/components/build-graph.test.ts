import { describe, expect, it } from "vitest";

import { buildGraph } from "./build-graph";

const b = (branch: string) => ({ branch });

describe("buildGraph", () => {
  it("spans a lane from a branch's newest build down to its oldest", () => {
    const { rows, width } = buildGraph([b("main"), b("main"), b("main")], "main");

    expect(width).toBe(14);
    expect(rows.map((row) => row.cells[0])).toEqual([
      { color: "var(--lane-1)", top: false, bottom: true, dot: true },
      { color: "var(--lane-1)", top: true, bottom: true, dot: true },
      { color: "var(--lane-1)", top: true, bottom: false, dot: true },
    ]);
  });

  it("keeps a recycled column distinct from the branch that vacated it", () => {
    // main holds lane 0 for rows 0-1; feature-x reuses that column from row 2.
    const { rows } = buildGraph([b("main"), b("main"), b("feature-x")], "main");

    expect(rows[2]!.cells[0]!.color).not.toBe(rows[0]!.cells[0]!.color);
    expect(rows[1]!.cells[0]!.bottom).toBe(false);
    expect(rows[2]!.cells[0]!.top).toBe(false);
  });

  it("continues open lanes off the bottom when the page was truncated", () => {
    const cut = buildGraph([b("main"), b("main")], "main", true);
    const exhausted = buildGraph([b("main"), b("main")], "main");

    expect(cut.rows[1]!.cells[0]!.bottom).toBe(true);
    expect(exhausted.rows[1]!.cells[0]!.bottom).toBe(false);
  });

  it("holds a lane open past its last visible build when the page was truncated", () => {
    // feature's only visible build is row 0, but the cut means older ones may exist
    // below the page — its lane must not terminate, nor free its column for reuse.
    const { rows, width } = buildGraph([b("feature"), b("main"), b("main")], "main", true);

    expect(width).toBe(28);
    expect(rows.map((row) => row.cells[1]!.bottom)).toEqual([true, true, true]);
    expect(rows[2]!.cells[1]!.dot).toBe(false);
    expect(rows[2]!.cells[1]!.color).toBe(rows[0]!.cells[1]!.color);
  });

  it("holds the trunk's lane open above the default branch's newest build", () => {
    const { rows, width } = buildGraph([b("topic"), b("main")], "main");

    expect(width).toBe(28);
    expect(rows[0]!.cells[0]).toBeNull();
    expect(rows[0]!.cells[1]!.dot).toBe(true);
    expect(rows[1]!.cells[0]!.dot).toBe(true);
  });

  it("draws nothing for an empty list", () => {
    expect(buildGraph([], "main")).toEqual({ rows: [], width: 0 });
  });
});
