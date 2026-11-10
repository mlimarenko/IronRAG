import { authApi } from '@/shared/api'
import type { TelemetryPreferencesApi } from './accountConsent'

export const telemetryPreferencesApi: TelemetryPreferencesApi = {
  load: () => authApi.getIamPreferences(),
  seed: (principalId, consent) => authApi.seedIamPreferences(principalId, consent),
  update: (principalId, consent) => authApi.updateIamPreferences(principalId, consent),
}
