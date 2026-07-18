import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Loader2, AlertCircle } from 'lucide-react'
import { Button } from '@/shared/components/ui/button'

export default function SwaggerPage() {
  const { t } = useTranslation()
  const [state, setState] = useState<'loading' | 'loaded' | 'error'>('loading')

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      {state === 'loading' && (
        <div className="flex items-center justify-center h-full">
          <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
        </div>
      )}
      {state === 'error' && (
        <div className="flex flex-col items-center justify-center h-full">
          <AlertCircle className="h-8 w-8 text-destructive mb-3" />
          <h2 className="text-base font-bold">{t('swagger.failedToLoadSpec')}</h2>
          <Button
            variant="outline"
            size="sm"
            className="mt-3"
            onClick={() => window.location.reload()}
          >
            {t('documents.retry')}
          </Button>
        </div>
      )}
      <iframe
        src="/swagger.html"
        title="IronRAG API Reference"
        className="flex-1 w-full border-0"
        onLoad={() => setState('loaded')}
        onError={() => setState('error')}
      />
    </div>
  )
}
