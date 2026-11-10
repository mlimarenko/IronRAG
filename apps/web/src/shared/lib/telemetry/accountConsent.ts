import {
  applyAccountTelemetryConsent,
  beginAccountTelemetryConsent,
  completeAccountTelemetryConsent,
  failAccountTelemetryConsent,
  readTelemetryConsentState,
  writeBrowserTelemetryConsent,
  type TelemetryConsent,
} from './consent'
import { telemetryPreferencesApi } from './preferencesApi'

interface AccountTelemetryPreference {
  telemetryConsent?: 'granted' | 'denied' | null
}

export interface TelemetryPreferencesApi {
  load(): Promise<AccountTelemetryPreference>
  seed(principalId: string, consent: 'granted' | 'denied'): Promise<AccountTelemetryPreference>
  update(principalId: string, consent: 'granted' | 'denied'): Promise<AccountTelemetryPreference>
}

let syncRevision = 0
let choiceRevision = 0
let pendingWrite: Promise<void> = Promise.resolve()

function isCurrentLoad(principalId: string, revision: number): boolean {
  const state = readTelemetryConsentState()
  return (
    syncRevision === revision && state.principalId === principalId && state.status === 'loading'
  )
}

/**
 * Load the signed-in account's preference. The browser's pre-sign-in choice is
 * captured and removed first, then used only to seed an unset account. A load
 * or seed failure leaves the account unknown, which keeps optional telemetry
 * off rather than falling back to another account's choice.
 */
export async function syncAccountTelemetryConsent(
  nextPrincipalId: string,
  api: TelemetryPreferencesApi = telemetryPreferencesApi,
): Promise<void> {
  const revision = ++syncRevision
  choiceRevision++
  const seedCandidate = beginAccountTelemetryConsent(nextPrincipalId)
  try {
    const current = await api.load()
    if (!isCurrentLoad(nextPrincipalId, revision)) return
    let consent = current.telemetryConsent ?? null
    if (consent === null && seedCandidate) {
      const seeded = await api.seed(nextPrincipalId, seedCandidate)
      if (!isCurrentLoad(nextPrincipalId, revision)) return
      consent = seeded.telemetryConsent ?? null
    }
    completeAccountTelemetryConsent(nextPrincipalId)
    applyAccountTelemetryConsent(nextPrincipalId, consent ?? 'unset')
  } catch {
    if (isCurrentLoad(nextPrincipalId, revision)) failAccountTelemetryConsent(nextPrincipalId)
  }
}

/**
 * Apply one explicit choice from the settings toggle or the first-run banner.
 * Disabling is immediate because it reduces collection; enabling waits for the
 * server to record the choice so a failed write cannot turn telemetry on.
 */
export async function requestTelemetryConsent(
  choice: Exclude<TelemetryConsent, 'unset'>,
  api: TelemetryPreferencesApi = telemetryPreferencesApi,
): Promise<void> {
  const state = readTelemetryConsentState()
  if (state.status === 'anonymous') {
    writeBrowserTelemetryConsent(choice)
    return
  }
  if (state.status !== 'ready' || !state.principalId) return
  const principalId = state.principalId
  const revision = ++choiceRevision
  const sessionRevision = syncRevision
  if (choice === 'denied') {
    applyAccountTelemetryConsent(principalId, 'denied')
  }
  const write = pendingWrite.then(async () => {
    const current = readTelemetryConsentState()
    if (
      syncRevision !== sessionRevision ||
      current.status !== 'ready' ||
      current.principalId !== principalId
    )
      return
    try {
      const updated = await api.update(principalId, choice)
      if (choiceRevision === revision) {
        applyAccountTelemetryConsent(principalId, updated.telemetryConsent ?? choice)
      }
    } catch {
      // Failed grants remain off; failed denials are already off locally.
    }
  })
  pendingWrite = write
  await write
}
