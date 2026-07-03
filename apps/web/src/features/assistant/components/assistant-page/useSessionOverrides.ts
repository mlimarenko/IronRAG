import { useCallback, useMemo, useState } from 'react';
import type { AssistantSession } from '@/shared/types';

/**
 * Client-side session rename + delete overrides.
 *
 * The backend query-session API today exposes only create / list / get / turn
 * — there is no rename or delete endpoint, and standing one up is out of scope
 * for the web redesign. To still give users meaningful session management we
 * persist two browser-local overlays, keyed per library scope:
 *
 *   - `titles`: a map of sessionId → user-chosen title (rename)
 *   - `hidden`: a set of sessionIds the user removed from their list (delete)
 *
 * These are applied on top of the server list in `applyOverrides`. They are
 * intentionally per-browser: they never mutate server state, so the
 * grounded-answer contract and durable conversation history are untouched. If
 * a real endpoint lands later, this overlay can be swapped for API mutations
 * without changing the rail's component contract.
 */

type SessionOverrides = {
  titles: Record<string, string>;
  hidden: string[];
};

const STORAGE_PREFIX = 'ironrag_assistant_session_overrides';

function storageKey(scopeKey: string | null): string | null {
  return scopeKey ? `${STORAGE_PREFIX}:${scopeKey}` : null;
}

function readOverrides(scopeKey: string | null): SessionOverrides {
  const key = storageKey(scopeKey);
  const empty: SessionOverrides = { titles: {}, hidden: [] };
  if (!key || typeof window === 'undefined') return empty;
  try {
    const raw = window.localStorage.getItem(key);
    if (!raw) return empty;
    const parsed = JSON.parse(raw) as Partial<SessionOverrides>;
    return {
      titles: parsed.titles && typeof parsed.titles === 'object' ? parsed.titles : {},
      hidden: Array.isArray(parsed.hidden) ? parsed.hidden : [],
    };
  } catch {
    return empty;
  }
}

function writeOverrides(scopeKey: string | null, value: SessionOverrides) {
  const key = storageKey(scopeKey);
  if (!key || typeof window === 'undefined') return;
  try {
    window.localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Persistence is best-effort; a blocked/full quota must not break the rail.
  }
}

export type UseSessionOverridesResult = {
  /** Apply rename + delete overlays to a server session list. */
  applyOverrides: (sessions: AssistantSession[]) => AssistantSession[];
  /** Set or clear a custom title for a session (empty string clears it). */
  renameSession: (sessionId: string, title: string) => void;
  /** Hide a session from the user's list (client-side delete). */
  deleteSession: (sessionId: string) => void;
};

export function useSessionOverrides(scopeKey: string | null): UseSessionOverridesResult {
  const [state, setState] = useState<{ scopeKey: string | null; value: SessionOverrides }>(() => ({
    scopeKey,
    value: readOverrides(scopeKey),
  }));

  // Derive the live overrides during render: when the library scope changes we
  // fall back to a fresh storage read until the next mutation rebinds state.
  // This mirrors the codebase `useScopedState` pattern (no setState-in-render).
  const overrides = state.scopeKey === scopeKey ? state.value : readOverrides(scopeKey);

  const update = useCallback(
    (mutate: (current: SessionOverrides) => SessionOverrides) => {
      setState((current) => {
        const base = current.scopeKey === scopeKey ? current.value : readOverrides(scopeKey);
        const next = mutate(base);
        writeOverrides(scopeKey, next);
        return { scopeKey, value: next };
      });
    },
    [scopeKey],
  );

  const renameSession = useCallback(
    (sessionId: string, title: string) => {
      const trimmed = title.trim();
      update((current) => {
        const titles = { ...current.titles };
        if (trimmed) titles[sessionId] = trimmed;
        else delete titles[sessionId];
        return { ...current, titles };
      });
    },
    [update],
  );

  const deleteSession = useCallback(
    (sessionId: string) => {
      update((current) => ({
        ...current,
        hidden: current.hidden.includes(sessionId)
          ? current.hidden
          : [...current.hidden, sessionId],
      }));
    },
    [update],
  );

  const hiddenSet = useMemo(() => new Set(overrides.hidden), [overrides.hidden]);

  const applyOverrides = useCallback(
    (sessions: AssistantSession[]): AssistantSession[] =>
      sessions
        .filter((session) => !hiddenSet.has(session.id))
        .map((session) => {
          const title = overrides.titles[session.id];
          return title ? { ...session, title } : session;
        }),
    [hiddenSet, overrides.titles],
  );

  return { applyOverrides, renameSession, deleteSession };
}
