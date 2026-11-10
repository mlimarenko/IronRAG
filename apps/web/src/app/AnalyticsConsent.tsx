import { BarChart3 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { requestTelemetryConsent } from '@/shared/lib/telemetry/accountConsent'
import {
  readTelemetryConsentState,
  subscribeTelemetryConsent,
  type TelemetryConsentState,
} from '@/shared/lib/telemetry/consent'
import { Button } from '@/shared/ui'

const initialConsentState = (): TelemetryConsentState => readTelemetryConsentState()

export function AnalyticsConsent() {
  const { t } = useTranslation()
  const [state, setState] = useState<TelemetryConsentState>(initialConsentState)

  useEffect(() => subscribeTelemetryConsent(() => setState(readTelemetryConsentState())), [])

  if (state.consent !== 'unset' || state.status === 'loading' || state.status === 'error') {
    return null
  }

  const decide = (value: 'granted' | 'denied') => {
    void requestTelemetryConsent(value)
  }

  return (
    <aside
      className="consent-banner"
      role="region"
      aria-label={t('analytics.consentTitle')}
      data-testid="analytics-consent"
    >
      <BarChart3 size={18} aria-hidden="true" />
      <div className="consent-banner__body">
        <strong>{t('analytics.consentTitle')}</strong>
        <p>{t('analytics.consentBody')}</p>
      </div>
      <div className="consent-banner__actions">
        <Button size="sm" onClick={() => decide('denied')}>
          {t('analytics.consentDecline')}
        </Button>
        <Button size="sm" variant="primary" onClick={() => decide('granted')}>
          {t('analytics.consentAllow')}
        </Button>
      </div>
    </aside>
  )
}
