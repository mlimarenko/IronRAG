/**
 * Minimal subsequence fuzzy matcher tuned for a command palette. No external
 * dependency — the candidate set is tiny (nav items + a handful of actions +
 * the workspace's libraries), so a hand-rolled scorer is both sufficient and
 * keeps the bundle lean.
 *
 * Scoring rewards: contiguous runs, matches at word starts / after separators,
 * and a prefix match on the whole string. A non-subsequence query returns
 * `null` so the caller can drop the item entirely.
 */
export interface FuzzyResult {
  score: number;
  /** Indices in the target that matched, for optional highlight rendering. */
  matched: number[];
}

const SEPARATORS = new Set([' ', '-', '_', '/', '.', ':']);

export function fuzzyMatch(query: string, target: string): FuzzyResult | null {
  const q = query.trim().toLowerCase();
  if (!q) return { score: 0, matched: [] };

  const t = target.toLowerCase();
  const matched: number[] = [];

  let score = 0;
  let qi = 0;
  let prevMatchIdx = -2;

  for (let ti = 0; ti < t.length && qi < q.length; ti += 1) {
    if (t[ti] !== q[qi]) continue;

    matched.push(ti);

    // Contiguous run bonus.
    if (ti === prevMatchIdx + 1) score += 6;
    else score += 1;

    // Word-boundary bonus (start of string or after a separator).
    const prevChar = t[ti - 1];
    if (ti === 0 || (prevChar !== undefined && SEPARATORS.has(prevChar))) score += 8;

    prevMatchIdx = ti;
    qi += 1;
  }

  // Not all query chars consumed → not a subsequence.
  if (qi < q.length) return null;

  // Whole-string prefix is the strongest signal.
  if (t.startsWith(q)) score += 16;
  // Shorter targets that fully matched rank slightly higher.
  score -= Math.max(0, t.length - q.length) * 0.1;

  return { score, matched };
}
