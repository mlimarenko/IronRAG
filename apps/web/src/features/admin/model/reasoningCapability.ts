/**
 * The exact provider/model reasoning capability a model catalog row confirms,
 * projected for the binding selector.
 *
 * This is pair knowledge, not a provider policy: it answers "may an operator
 * select a mode for this exact model". The value lives only in the model
 * catalog entry's `metadataJson.reasoningCapability`, so a provider-wide or
 * client-supplied JSON field can never grant it. The server re-validates every
 * selection against the same catalog row (IRG-153); this projection only
 * decides what the UI may offer and explain.
 *
 * The UI never reads the reserved `_providerModelReasoning` projection or any
 * other request-policy field for capabilities — a client binding could forge
 * those, and the server discards them anyway.
 */

/** The provider runtime kind that has a confirmed reasoning wire adapter. */
export const OPENAI_COMPATIBLE_RUNTIME_KIND = 'openai_compatible'

/** Documented wire controls that apply the only currently selectable mode. */
const REASONING_WIRES = ['reasoning_effort_none', 'thinking_disabled'] as const
export type ReasoningWire = (typeof REASONING_WIRES)[number]

/** The one mode the current vocabulary confirms. */
export const REASONING_MODE_NONE = 'none'
export type ReasoningMode = typeof REASONING_MODE_NONE

export type ReasoningCapability =
  | { state: 'confirmed'; mode: ReasoningMode; wire: ReasoningWire; evidence: string }
  /** The exact pair states nothing: no mode may be selected. */
  | { state: 'unconfirmed' }
  /** The catalog metadata is present but not a readable confirmation. */
  | { state: 'invalid' }

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value))
}

function isWire(value: unknown): value is ReasoningWire {
  return typeof value === 'string' && (REASONING_WIRES as readonly string[]).includes(value)
}

/**
 * Read the pair capability out of one model's `metadataJson`.
 *
 * Mirrors the server's serde shape: an absent key is the honest `unconfirmed`
 * state, the unit variant serialises as the `"unconfirmed"` string, and the
 * struct variant is a single-key map `{ confirmed: { mode, wire, evidence } }`.
 * A present-but-unreadable value is `invalid` rather than quietly treated as
 * unset, so the UI shows the operator the real state instead of an empty
 * selector.
 */
export function parseReasoningCapability(metadataJson: unknown): ReasoningCapability {
  if (!isRecord(metadataJson)) return { state: 'unconfirmed' }
  const raw = metadataJson.reasoningCapability
  if (raw === undefined || raw === 'unconfirmed') return { state: 'unconfirmed' }
  if (!isRecord(raw)) return { state: 'invalid' }
  const keys = Object.keys(raw)
  if (keys.length !== 1 || keys[0] !== 'confirmed') return { state: 'invalid' }
  const confirmed = raw.confirmed
  if (!isRecord(confirmed)) return { state: 'invalid' }
  const { mode, wire, evidence } = confirmed
  if (mode !== REASONING_MODE_NONE) return { state: 'invalid' }
  if (!isWire(wire)) return { state: 'invalid' }
  if (typeof evidence !== 'string' || evidence.trim() === '') return { state: 'invalid' }
  return { state: 'confirmed', mode: REASONING_MODE_NONE, wire, evidence: evidence.trim() }
}

/** The modes an operator may select for a capability. Empty grants nothing. */
export function selectableReasoningModes(capability: ReasoningCapability): ReasoningMode[] {
  return capability.state === 'confirmed' ? [REASONING_MODE_NONE] : []
}
