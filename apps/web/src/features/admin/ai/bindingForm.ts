import type { AiBindingPurpose } from '@/shared/api/generated'
import type { AIBinding, AIBindingState } from '@/shared/types'
import {
  REASONING_MODE_NONE,
  selectableReasoningModes,
  type ReasoningCapability,
  type ReasoningWire,
  type ReasoningMode,
} from '../model/reasoningCapability'

/**
 * The canonical key a binding's typed reasoning selection occupies inside
 * `extra_parameters_json`. It is the one field the runtime reads as a typed
 * selection, so the raw JSON control must never be a second author of it.
 */
const MANAGED_REASONING_MODE_KEY = 'reasoningMode'
/** Version stamped into every typed selected-mode wire form. */
const MANAGED_REASONING_MODE_VERSION = 1

/** `''` is the honest unset state; `'none'` is the only selectable mode. */
export type ManagedReasoningMode = '' | ReasoningMode

/**
 * What a binding's stored JSON held for the managed key before the form owned
 * it. `invalid` is not repaired or read as unset: it is shown to the operator
 * and only an explicit Unset resolves it.
 */
export type StoredReasoningMode =
  { kind: 'unset' } | { kind: 'selected'; mode: ReasoningMode } | { kind: 'invalid' }

export type BindingForm = {
  accountId: string
  modelId: string
  systemPrompt: string
  temperature: string
  topP: string
  maxOutputTokens: string
  extraJson: string
  state: AIBindingState
  /** The typed selection the selector owns, `''` for unset. */
  reasoningMode: ManagedReasoningMode
  /** What the saved binding carried under the managed key. */
  reasoningStored: StoredReasoningMode
  /** The exact account/model the operator accepted; a changed target clears it. */
  reasoningTarget: { accountId: string; modelId: string } | null
}

export const EMPTY_BINDING_FORM: BindingForm = {
  accountId: '',
  modelId: '',
  systemPrompt: '',
  temperature: '',
  topP: '',
  maxOutputTokens: '',
  extraJson: '',
  state: 'active',
  reasoningMode: '',
  reasoningStored: { kind: 'unset' },
  reasoningTarget: null,
}

function parseExtra(extraJson: string): Record<string, unknown> | null {
  if (!extraJson.trim()) return {}
  try {
    const parsed = JSON.parse(extraJson) as unknown
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null
  } catch {
    return null
  }
}

/** Read the typed selection out of a raw `extraParametersJson` value. */
export function parseStoredReasoningMode(value: unknown): StoredReasoningMode {
  if (value === undefined) return { kind: 'unset' }
  if (!value || typeof value !== 'object' || Array.isArray(value)) return { kind: 'invalid' }
  const record = value as Record<string, unknown>
  const keys = Object.keys(record)
  if (keys.length !== 2 || !('version' in record) || !('mode' in record)) return { kind: 'invalid' }
  if (record.version !== MANAGED_REASONING_MODE_VERSION) return { kind: 'invalid' }
  if (record.mode !== REASONING_MODE_NONE) return { kind: 'invalid' }
  return { kind: 'selected', mode: REASONING_MODE_NONE }
}

/**
 * The form's JSON with the managed key removed, plus what it held. The raw
 * editor never shows or owns `reasoningMode`; the selector does.
 */
export function withoutManagedReasoningMode(extraJson: string): {
  extraJson: string
  stored: StoredReasoningMode
} {
  const extra = parseExtra(extraJson)
  if (extra === null || !(MANAGED_REASONING_MODE_KEY in extra)) {
    return { extraJson, stored: { kind: 'unset' } }
  }
  const { [MANAGED_REASONING_MODE_KEY]: raw, ...rest } = extra
  return {
    extraJson: Object.keys(rest).length === 0 ? '' : JSON.stringify(rest, null, 2),
    stored: parseStoredReasoningMode(raw),
  }
}

/** Merge the selector's typed selection into parsed extra parameters. */
export function applyManagedReasoningMode(
  extra: Record<string, unknown>,
  mode: ManagedReasoningMode,
): Record<string, unknown> {
  const next = { ...extra }
  if (mode === '') delete next[MANAGED_REASONING_MODE_KEY]
  else next[MANAGED_REASONING_MODE_KEY] = { version: MANAGED_REASONING_MODE_VERSION, mode }
  return next
}

/** Whether the raw JSON currently carries the managed key itself. */
export function hasManagedReasoningMode(extraJson: string): boolean {
  const extra = parseExtra(extraJson)
  return extra !== null && MANAGED_REASONING_MODE_KEY in extra
}

/** Remove one raw key from the form's JSON; unchanged when the JSON is invalid. */
export function withoutExtraParameter(extraJson: string, parameter: string): string {
  const extra = parseExtra(extraJson)
  if (extra === null || !(parameter in extra)) return extraJson
  const next = { ...extra }
  delete next[parameter]
  return Object.keys(next).length === 0 ? '' : JSON.stringify(next, null, 2)
}

/**
 * A raw parameter that disagrees with the confirmed wire control. Mirrors the
 * server's `raw_conflict` exactly: the declared control may appear with its
 * exact value, while the other owned switch is a conflict even when it also
 * disables reasoning. `other_control` names a foreign owned switch that is
 * present; `disagreeing_value` names the declared switch carrying the wrong
 * value.
 */
export type RawReasoningConflict = {
  parameter: 'reasoning_effort' | 'thinking'
  kind: 'other_control' | 'disagreeing_value'
}

const WIRE_OWNED_PARAMETER: Record<ReasoningWire, 'reasoning_effort' | 'thinking'> = {
  reasoning_effort_none: 'reasoning_effort',
  thinking_disabled: 'thinking',
}

function isThinkingDisabled(value: unknown): boolean {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false
  const record = value as Record<string, unknown>
  return record.type === 'disabled' && Object.keys(record).length === 1
}

export function rawReasoningConflict(
  wire: ReasoningWire,
  extraJson: string,
): RawReasoningConflict | null {
  const extra = parseExtra(extraJson)
  if (extra === null) return null
  const owned = WIRE_OWNED_PARAMETER[wire]
  const other = owned === 'reasoning_effort' ? 'thinking' : 'reasoning_effort'
  if (other in extra) return { parameter: other, kind: 'other_control' }
  if (!(owned in extra)) return null
  const agrees =
    wire === 'reasoning_effort_none' ? extra[owned] === 'none' : isThinkingDisabled(extra[owned])
  return agrees ? null : { parameter: owned, kind: 'disagreeing_value' }
}

/** Which explanation the read-only reasoning field shows. */
type ReasoningReadonlyKey = 'invalidCapability' | 'runtimeUnsupported' | 'unconfirmed'

/**
 * Everything the binding dialog derives from the exact selected model's
 * capability and the form's own state. Kept as one pure function so the
 * component reads as a render, not as a decision tree, and so the decision
 * table can be tested without mounting the dialog.
 */
export type ReasoningJournal = {
  /** A selectable mode exists for this exact pair and runtime. */
  confirmed: boolean
  /** The confirmed wire control; `null` when nothing is confirmed. */
  wire: ReasoningWire | null
  /** Modes the selector may offer; empty grants nothing. */
  modes: ReasoningMode[]
  /** A raw parameter that disagrees with the confirmed control. */
  rawConflict: RawReasoningConflict | null
  /** The stored JSON held an unreadable managed key. */
  storedInvalid: boolean
  /** A pending mode is not (or no longer) confirmed for the current pair. */
  modeUnsupported: boolean
  /** The pair the operator accepted is not the pair the form now targets. */
  targetChanged: boolean
  /** The raw JSON itself carries the managed key. */
  managedKeyTyped: boolean
  /** The field must be visible so a blocked save can name its reason. */
  show: boolean
  /** Saving is blocked until the operator resolves the reasoning state. */
  blocked: boolean
  /** Which read-only explanation the field shows when nothing is confirmed. */
  readonlyKey: ReasoningReadonlyKey
}

function reasoningReadonlyKey(
  capability: ReasoningCapability | undefined,
  runtimeCapable: boolean,
): ReasoningReadonlyKey {
  if (capability?.state === 'invalid') return 'invalidCapability'
  if (capability?.state === 'confirmed' && !runtimeCapable) return 'runtimeUnsupported'
  return 'unconfirmed'
}

export function deriveReasoningJournal(input: {
  purpose: AiBindingPurpose | null
  hasSelectedAccount: boolean
  capability: ReasoningCapability | undefined
  runtimeCapable: boolean
  form: BindingForm
}): ReasoningJournal {
  const { purpose, hasSelectedAccount, capability, runtimeCapable, form } = input
  const confirmed = capability?.state === 'confirmed' && runtimeCapable
  const wire = capability?.state === 'confirmed' ? capability.wire : null
  const modes = capability ? selectableReasoningModes(capability) : []
  const rawConflict =
    confirmed && wire && form.reasoningMode !== ''
      ? rawReasoningConflict(wire, form.extraJson)
      : null
  const storedInvalid = form.reasoningStored.kind === 'invalid'
  const modeUnsupported = form.reasoningMode !== '' && !confirmed
  const targetChanged =
    form.reasoningMode !== '' &&
    (form.reasoningTarget?.accountId !== form.accountId ||
      form.reasoningTarget?.modelId !== form.modelId)
  const managedKeyTyped = hasManagedReasoningMode(form.extraJson)
  const show =
    hasSelectedAccount &&
    (purpose !== 'embed_chunk' || form.reasoningMode !== '' || storedInvalid || managedKeyTyped)
  const blocked =
    show &&
    (storedInvalid || modeUnsupported || targetChanged || Boolean(rawConflict) || managedKeyTyped)
  return {
    confirmed,
    wire,
    modes,
    rawConflict,
    storedInvalid,
    modeUnsupported,
    targetChanged,
    managedKeyTyped,
    show,
    blocked,
    readonlyKey: reasoningReadonlyKey(capability, runtimeCapable),
  }
}

export function formFromBinding(binding: AIBinding): BindingForm {
  const { extraJson, stored } = withoutManagedReasoningMode(
    binding.extraParams ? JSON.stringify(binding.extraParams, null, 2) : '',
  )
  return {
    accountId: binding.accountId,
    modelId: binding.modelCatalogId,
    systemPrompt: binding.systemPrompt ?? '',
    temperature: binding.temperature == null ? '' : String(binding.temperature),
    topP: binding.topP == null ? '' : String(binding.topP),
    maxOutputTokens: binding.maxOutputTokens == null ? '' : String(binding.maxOutputTokens),
    extraJson,
    state: binding.state,
    reasoningMode: stored.kind === 'selected' ? stored.mode : '',
    reasoningStored: stored,
    reasoningTarget:
      stored.kind === 'selected'
        ? { accountId: binding.accountId, modelId: binding.modelCatalogId }
        : null,
  }
}

/**
 * Request tuning the binding form edits through dedicated controls. The
 * additional-parameters JSON stays the one source of truth: each control reads
 * and writes exactly one key of it, and an empty choice removes the key so the
 * provider default applies.
 */
const REQUEST_TUNING_KEYS = {
  reasoningEffort: 'reasoning_effort',
  serviceTier: 'service_tier',
} as const

export type RequestTuningKey = keyof typeof REQUEST_TUNING_KEYS

export const REASONING_EFFORTS = ['none', 'low', 'medium', 'high'] as const
export const SERVICE_TIERS = ['flex'] as const

/** The tuning value in the form's JSON, `''` when absent, `null` when the JSON is invalid. */
export function requestTuningValue(extraJson: string, key: RequestTuningKey): string | null {
  const extra = parseExtra(extraJson)
  if (extra === null) return null
  const value = extra[REQUEST_TUNING_KEYS[key]]
  return typeof value === 'string' ? value : ''
}

/** The form's JSON with one tuning key set, or removed for `''`; unchanged when invalid. */
export function withRequestTuning(extraJson: string, key: RequestTuningKey, value: string): string {
  const extra = parseExtra(extraJson)
  if (extra === null) return extraJson
  const next = { ...extra }
  if (value) next[REQUEST_TUNING_KEYS[key]] = value
  else delete next[REQUEST_TUNING_KEYS[key]]
  return Object.keys(next).length === 0 ? '' : JSON.stringify(next, null, 2)
}

/** Tuning shown on a binding card: only the keys the dedicated controls own. */
export function bindingRequestTuning(
  extraParams: Record<string, unknown> | undefined,
): Partial<Record<RequestTuningKey, string>> {
  const tuning: Partial<Record<RequestTuningKey, string>> = {}
  for (const key of Object.keys(REQUEST_TUNING_KEYS) as RequestTuningKey[]) {
    const value = extraParams?.[REQUEST_TUNING_KEYS[key]]
    if (typeof value === 'string' && value) tuning[key] = value
  }
  return tuning
}
