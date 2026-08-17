/**
 * A `git log --graph`-style lane column for the build list.
 *
 * ponytail: builds only store their own commit SHA — no parent SHAs — so the lanes
 * group builds by branch instead of following real ancestry: one lane per branch,
 * spanning from its newest build down to its oldest, with no merge/fork edges
 * between lanes. Store parent SHAs on the build to draw a true ancestry graph.
 */

const LANE_WIDTH = 14;
const STROKE = 2;
const DOT_RADIUS = 4;

/** Lane colors, cycled by lane index like every other git graph does. */
const LANE_COLORS = [
  "oklch(0.62 0.19 259)",
  "oklch(0.68 0.17 145)",
  "oklch(0.72 0.18 65)",
  "oklch(0.65 0.22 15)",
  "oklch(0.62 0.2 305)",
  "oklch(0.7 0.14 195)",
];

/** One lane at one row: a vertical segment above and/or below the row's midpoint. */
type Cell = { branch: string; top: boolean; bottom: boolean; dot: boolean };

export type GraphRow = { cells: (Cell | null)[] };

/**
 * Lay out one lane per branch over a newest-first build list.
 *
 * A lane opens at a branch's newest build, stays occupied down to its oldest one,
 * then frees its column for a later branch. The default branch, when present, keeps
 * column 0 so the trunk always reads as the leftmost lane.
 */
export function buildGraph(
  builds: { branch: string }[],
  defaultBranch?: string,
): { rows: GraphRow[]; width: number } {
  const first = new Map<string, number>();
  const last = new Map<string, number>();
  builds.forEach((build, i) => {
    if (!first.has(build.branch)) first.set(build.branch, i);
    last.set(build.branch, i);
  });

  const active: (string | null)[] = [];
  const open = (branch: string) => {
    const free = active.indexOf(null);
    const lane = free === -1 ? active.length : free;
    active[lane] = branch;
    return lane;
  };
  if (defaultBranch && first.has(defaultBranch)) open(defaultBranch);

  let width = 0;
  const rows = builds.map((build, i) => {
    if (!active.includes(build.branch)) open(build.branch);
    const cells = active.map((branch) => {
      // A reserved-but-not-yet-started lane (the default branch below its newest
      // build) draws nothing until its own first row.
      if (branch === null || i < first.get(branch)!) return null;
      return {
        branch,
        top: i > first.get(branch)!,
        bottom: i < last.get(branch)!,
        dot: branch === build.branch,
      };
    });
    width = Math.max(width, active.length);
    active.forEach((branch, lane) => {
      if (branch !== null && last.get(branch) === i) active[lane] = null;
    });
    return { cells };
  });

  return { rows, width: width * LANE_WIDTH };
}

/** Renders one row of {@link buildGraph}, filling its (relatively positioned) cell. */
export function BuildGraph({ row, branch }: { row: GraphRow; branch: string }) {
  return (
    <svg className="absolute inset-0 h-full" role="img" aria-label={`branch ${branch}`}>
      {row.cells.map((cell, lane) => {
        if (cell === null) return null;
        const x = lane * LANE_WIDTH + LANE_WIDTH / 2;
        const color = LANE_COLORS[lane % LANE_COLORS.length];
        return (
          <g key={lane} stroke={color} strokeWidth={STROKE} fill={color}>
            {cell.top ? <line x1={x} x2={x} y1="0" y2="50%" /> : null}
            {cell.bottom ? <line x1={x} x2={x} y1="50%" y2="100%" /> : null}
            {cell.dot ? <circle cx={x} cy="50%" r={DOT_RADIUS} stroke="none" /> : null}
          </g>
        );
      })}
    </svg>
  );
}
