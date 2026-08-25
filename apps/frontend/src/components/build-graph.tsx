/**
 * A `git log --graph`-style lane column for the build list.
 *
 * This is VRT baseline ancestry, not inferred Git history. Every completed build
 * points at the approved build whose baseline it actually compared against. A
 * cross-branch pointer is rendered as a diagonal join into that source build.
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

/** Lanes are capped at the palette size so the column stays narrow. */
const MAX_LANES = LANE_COLORS.length;

/** One lane at one row: a vertical segment above and/or below the row's midpoint. */
type Cell = {
  color: string;
  top: boolean;
  bottom: boolean;
  dot: boolean;
  /** Set when a laneless branch borrows this column for its dot; overrides `color` for the dot only. */
  dotColor?: string;
  /** At this row, join this lane into another lane's build dot. */
  joinTo?: number;
};

export type Cells = (Cell | null)[];
export type GraphRow<T> = { build: T; cells: Cells };

/**
 * Lay out one lane per branch over a newest-first build list.
 *
 * A lane opens at a branch's newest build. Its lower end comes from the oldest
 * visible build's `baseline_source`: it joins the source build when that build is
 * visible, continues below the page when the source is older/deleted, and otherwise
 * terminates at the build dot. The default branch, when present, opens first so the
 * trunk starts leftmost. Columns get recycled only after that truthful endpoint.
 *
 * Lanes are capped at {@link MAX_LANES}: once every column is occupied, further
 * branches get no lane of their own — their builds are drawn as a lone dot on the
 * rightmost column. This bounds the column width even when several long-lived
 * branches overlap.
 */
export function buildGraph<
  T extends {
    id: string;
    number: number;
    branch: string;
    baseline_source?: {
      branch: string;
      build_id?: string | null;
      build_number?: number | null;
    } | null;
  },
>(builds: T[], defaultBranch?: string): { rows: GraphRow<T>[]; width: number } {
  const first = new Map<string, number>();
  const last = new Map<string, number>();
  const indexById = new Map<string, number>();
  builds.forEach((build, i) => {
    if (!first.has(build.branch)) first.set(build.branch, i);
    last.set(build.branch, i);
    indexById.set(build.id, i);
  });

  // A branch remains open through the row where its oldest visible build joins
  // the baseline source. When that source is outside retained/visible history,
  // carry the lane to the page boundary instead of inventing an endpoint.
  const end = new Map(last);
  const joins = new Map<number, { from: string; to: string }[]>();
  for (const [branch, lastRow] of last) {
    const build = builds[lastRow]!;
    const source = build.baseline_source;
    if (!source) continue;

    const sourceRow = source.build_id ? indexById.get(source.build_id) : undefined;
    if (sourceRow !== undefined && sourceRow > lastRow) {
      end.set(branch, sourceRow);
      if (source.branch !== branch) {
        const rowJoins = joins.get(sourceRow) ?? [];
        rowJoins.push({ from: branch, to: source.branch });
        joins.set(sourceRow, rowJoins);
      }
      continue;
    }

    const sourceIsOlder =
      source.build_number === null ||
      source.build_number === undefined ||
      source.build_number < build.number;
    if (sourceRow === undefined && sourceIsOlder && builds.length > 0) {
      // One row beyond the last visible row keeps the final half-segment open,
      // explicitly showing that the source lies below this page.
      end.set(branch, builds.length);
    }
  }

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
  // The row where each branch actually got its lane. Usually its first build's row,
  // but later for a branch that spent time laneless waiting for a column to free —
  // its line must start at the acquired row, not reach up toward its borrowed dots.
  const opened = new Map<string, number>();
  const open = (branch: string, row: number) => {
    const free = active.indexOf(null);
    if (free !== -1) active[free] = branch;
    else if (active.length < MAX_LANES) active.push(branch);
    else return; // no lane left — the branch's builds render as dots on the rightmost column
    opened.set(branch, row);
  };
  if (defaultBranch && first.has(defaultBranch)) {
    colorOf(defaultBranch);
    open(defaultBranch, first.get(defaultBranch)!);
  }

  let width = 0;
  const rows = builds.map((build, i) => {
    if (!active.includes(build.branch)) open(build.branch, i);
    const laneless = !active.includes(build.branch);
    const cells = active.map((branch, lane): Cell | null => {
      // A reserved-but-not-yet-started lane (the default branch below its newest
      // build) draws nothing until its own first row.
      if (branch === null || i < first.get(branch)!) return null;
      // A laneless build borrows the rightmost lane: the owner's line runs through,
      // with the borrower's dot on top.
      const borrowed = laneless && lane === active.length - 1;
      return {
        color: colorOf(branch),
        top: i > opened.get(branch)!,
        bottom: i < end.get(branch)!,
        dot: borrowed || branch === build.branch,
        ...(borrowed ? { dotColor: colorOf(build.branch) } : null),
      };
    });
    width = Math.max(width, active.length);
    for (const join of joins.get(i) ?? []) {
      const fromLane = active.indexOf(join.from);
      const toLane = active.indexOf(join.to);
      if (fromLane === -1 || toLane === -1) continue;
      const cell = cells[fromLane];
      if (cell) cell.joinTo = toLane;
    }
    active.forEach((branch, lane) => {
      if (branch !== null && end.get(branch) === i) active[lane] = null;
    });
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
            {cell.joinTo !== undefined ? (
              <line x1={x} x2={cell.joinTo * LANE_WIDTH + LANE_WIDTH / 2} y1="0" y2="50%" />
            ) : cell.top ? (
              <line x1={x} x2={x} y1="0" y2="50%" />
            ) : null}
            {cell.bottom ? <line x1={x} x2={x} y1="50%" y2="100%" /> : null}
            {cell.dot ? (
              // A borrowed dot gets a card-colored ring so it stays visible even when
              // the color cycle hands it the same color as the lane it sits on.
              <circle
                cx={x}
                cy="50%"
                r={DOT_RADIUS}
                fill={cell.dotColor ?? cell.color}
                stroke={cell.dotColor ? "var(--card)" : "none"}
              />
            ) : null}
          </g>
        );
      })}
    </svg>
  );
}
