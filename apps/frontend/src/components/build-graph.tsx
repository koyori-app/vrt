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

/** Lane colors, assigned per branch. Defined in styles.css so both themes can tune them. */
const LANE_COLORS = [
  "var(--lane-1)",
  "var(--lane-2)",
  "var(--lane-3)",
  "var(--lane-4)",
  "var(--lane-5)",
  "var(--lane-6)",
];

/** One lane at one row: a vertical segment above and/or below the row's midpoint. */
type Cell = { color: string; top: boolean; bottom: boolean; dot: boolean };

export type Cells = (Cell | null)[];
export type GraphRow<T> = { build: T; cells: Cells };

/**
 * Lay out one lane per branch over a newest-first build list.
 *
 * A lane opens at a branch's newest build, stays occupied down to its oldest one,
 * then frees its column for a later branch. The default branch, when present, opens
 * first so the trunk starts leftmost. Columns get recycled, so color is keyed by
 * branch rather than by column — two unrelated branches sharing a column stay
 * visually distinct.
 *
 * `truncated` says the list was cut off by a fetch limit rather than exhausted. Only
 * an exhausted list proves a branch ended: past the cut, any branch on the page may
 * still have older builds. So a truncated page holds every lane open to the bottom
 * and never recycles a column — a branch whose visible builds stopped early is drawn
 * as continuing, not as terminated.
 *
 * ponytail: total count is all we have to go on. Return `has_older_builds` per branch
 * from the list endpoint to close the lanes that really did end.
 */
export function buildGraph<T extends { branch: string }>(
  builds: T[],
  defaultBranch?: string,
  truncated = false,
): { rows: GraphRow<T>[]; width: number } {
  const first = new Map<string, number>();
  const last = new Map<string, number>();
  builds.forEach((build, i) => {
    if (!first.has(build.branch)) first.set(build.branch, i);
    last.set(build.branch, i);
  });

  const colors = new Map<string, string>();
  const colorOf = (branch: string) => {
    let color = colors.get(branch);
    if (color === undefined) {
      color = LANE_COLORS[colors.size % LANE_COLORS.length]!;
      colors.set(branch, color);
    }
    return color;
  };

  const active: (string | null)[] = [];
  const open = (branch: string) => {
    const free = active.indexOf(null);
    active[free === -1 ? active.length : free] = branch;
  };
  if (defaultBranch && first.has(defaultBranch)) {
    colorOf(defaultBranch);
    open(defaultBranch);
  }

  let width = 0;
  const rows = builds.map((build, i) => {
    if (!active.includes(build.branch)) open(build.branch);
    const cells = active.map((branch) => {
      // A reserved-but-not-yet-started lane (the default branch below its newest
      // build) draws nothing until its own first row.
      if (branch === null || i < first.get(branch)!) return null;
      return {
        color: colorOf(branch),
        top: i > first.get(branch)!,
        bottom: truncated || i < last.get(branch)!,
        dot: branch === build.branch,
      };
    });
    width = Math.max(width, active.length);
    if (!truncated) {
      active.forEach((branch, lane) => {
        if (branch !== null && last.get(branch) === i) active[lane] = null;
      });
    }
    return { build, cells };
  });

  return { rows, width: width * LANE_WIDTH };
}

/**
 * Renders one row of {@link buildGraph}, filling its (relatively positioned) cell.
 * Decorative: the Branch column next to it already names the branch.
 */
export function BuildGraph({ cells }: { cells: Cells }) {
  return (
    <svg className="absolute inset-0 h-full" aria-hidden="true">
      {cells.map((cell, lane) => {
        if (cell === null) return null;
        const x = lane * LANE_WIDTH + LANE_WIDTH / 2;
        return (
          <g key={lane} stroke={cell.color} strokeWidth={STROKE} fill={cell.color}>
            {cell.top ? <line x1={x} x2={x} y1="0" y2="50%" /> : null}
            {cell.bottom ? <line x1={x} x2={x} y1="50%" y2="100%" /> : null}
            {cell.dot ? <circle cx={x} cy="50%" r={DOT_RADIUS} stroke="none" /> : null}
          </g>
        );
      })}
    </svg>
  );
}
