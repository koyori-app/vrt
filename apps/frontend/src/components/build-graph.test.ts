import { describe, expect, it } from "vitest";

import { buildGraph } from "./build-graph";

type Source = {
  branch: string;
  build_id: string | null;
  build_number: number | null;
};

const b = (number: number, branch: string, baseline_source: Source | null = null) => ({
  id: `build-${number}`,
  number,
  branch,
  baseline_source,
});

const source = (number: number, branch: string): Source => ({
  branch,
  build_id: `build-${number}`,
  build_number: number,
});

describe("buildGraph", () => {
  it("spans a lane from a branch's newest build down to its root", () => {
    const { rows, width } = buildGraph([b(3, "main"), b(2, "main"), b(1, "main")], "main");

    expect(width).toBe(14);
    expect(rows.map((row) => row.cells[0])).toEqual([
      { color: "var(--lane-1)", top: false, bottom: true, dot: true },
      { color: "var(--lane-1)", top: true, bottom: true, dot: true },
      { color: "var(--lane-1)", top: true, bottom: false, dot: true },
    ]);
  });

  it("terminates a single-build branch instead of drawing an endless ponytail", () => {
    const { rows, width } = buildGraph([b(2, "feature-a"), b(1, "feature-b")], "main");

    expect(width).toBe(14);
    expect(rows[0]!.cells[0]).toMatchObject({ top: false, bottom: false, dot: true });
    expect(rows[1]!.cells[0]).toMatchObject({ top: false, bottom: false, dot: true });
    expect(rows[1]!.cells[0]!.color).not.toBe(rows[0]!.cells[0]!.color);
  });

  it("joins a branch into the visible build its baseline came from", () => {
    const builds = [b(5, "feature", source(2, "main")), b(4, "other"), b(3, "main"), b(2, "main")];
    const { rows, width } = buildGraph(builds, "main");

    expect(width).toBe(42);
    expect(rows[0]!.cells[1]).toMatchObject({ top: false, bottom: true, dot: true });
    expect(rows[3]!.cells[1]).toMatchObject({
      top: true,
      bottom: false,
      dot: false,
      joinTo: 0,
    });
    expect(rows[3]!.cells[0]).toMatchObject({ dot: true });
  });

  it("continues below the page when the exact source build is older than the page", () => {
    const { rows } = buildGraph([b(5, "feature", source(1, "main")), b(4, "main")], "main");

    expect(rows[1]!.cells[1]).toMatchObject({ top: true, bottom: true, dot: false });
  });

  it("continues when retention deleted the baseline's source build", () => {
    const deleted: Source = { branch: "main", build_id: null, build_number: null };
    const { rows } = buildGraph([b(5, "feature", deleted), b(4, "main")], "main");

    expect(rows[1]!.cells[1]).toMatchObject({ bottom: true, dot: false });
  });

  it("caps overlapping off-page ancestry at the palette size", () => {
    const builds = Array.from({ length: 9 }, (_, i) =>
      b(20 - i, `branch-${i}`, {
        branch: "main",
        build_id: "build-1",
        build_number: 1,
      }),
    );
    const { rows, width } = buildGraph(builds);

    expect(width).toBe(84);
    for (const row of rows) expect(row.cells.length).toBeLessThanOrEqual(6);
    const overflow = rows[7]!.cells[5]!;
    expect(overflow.dot).toBe(true);
    expect(overflow.dotColor).toBe("var(--lane-2)");
  });

  it("draws nothing for an empty list", () => {
    expect(buildGraph([], "main")).toEqual({ rows: [], width: 0 });
  });
});
