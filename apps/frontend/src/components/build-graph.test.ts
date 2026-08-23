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

  it("caps the lanes at the palette size and dots overflow branches on the last column", () => {
    // 9 branches on a truncated page: no lane ever frees, so branches 7-9 overflow.
    const builds = Array.from({ length: 9 }, (_, i) => b(`branch-${i}`));
    const { rows, width } = buildGraph(builds, undefined, true);

    expect(width).toBe(84);
    // Cells grow with the opened lanes but never past the cap.
    for (const row of rows) expect(row.cells.length).toBeLessThanOrEqual(6);
    expect(rows[8]!.cells).toHaveLength(6);
    // An overflow branch's row borrows the rightmost column: the lane owner's line
    // continues through it, with the overflow branch's own dot color on top.
    // branch-7 is the 8th branch colored, so the cycle hands it lane color 2.
    const overflow = rows[7]!.cells[5]!;
    expect(overflow.dot).toBe(true);
    expect(overflow.color).toBe("var(--lane-6)");
    expect(overflow.dotColor).toBe("var(--lane-2)");
    expect(overflow.top).toBe(true);
    expect(overflow.bottom).toBe(true);
    // A laned branch's row is unaffected.
    expect(rows[3]!.cells[3]!.dot).toBe(true);
    expect(rows[3]!.cells[3]!.dotColor).toBeUndefined();
  });

  it("starts a late-acquired lane at the acquiring row instead of reaching up", () => {
    // 7 branches × 2 builds: branch-6 is laneless at row 6 (all 6 lanes busy), then
    // takes the column branch-0 frees. Its line must start where it got the lane —
    // no upward segment pointing at rows where the column was someone else's.
    const branches = Array.from({ length: 7 }, (_, n) => b(`branch-${n}`));
    const { rows } = buildGraph([...branches, ...branches]);

    expect(rows[6]!.cells[5]!.dotColor).toBeDefined();
    expect(rows[13]!.cells[0]!.dot).toBe(true);
    expect(rows[13]!.cells[0]!.dotColor).toBeUndefined();
    expect(rows[13]!.cells[0]!.top).toBe(false);
  });

  it("draws nothing for an empty list", () => {
    expect(buildGraph([], "main")).toEqual({ rows: [], width: 0 });
  });
});
