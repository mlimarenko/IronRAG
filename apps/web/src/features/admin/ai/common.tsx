import { AlertTriangle, LoaderCircle } from 'lucide-react'
import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'

import { Button } from '@/shared/ui'

export function DataBoundary({
  loading,
  error,
  onRetry,
  children,
}: Readonly<{
  loading: boolean
  error: unknown
  onRetry: () => void
  children: ReactNode
}>) {
  const { t } = useTranslation()
  if (loading) {
    return (
      <div className="state-card aicfg-loading-state" aria-live="polite">
        <LoaderCircle className="spin" size={22} />
      </div>
    )
  }
  if (error) {
    return (
      <div className="state-card state-card--error" role="alert">
        <AlertTriangle size={22} />
        <Button size="sm" onClick={onRetry}>
          {t('common.tryAgain')}
        </Button>
      </div>
    )
  }
  return <>{children}</>
}
