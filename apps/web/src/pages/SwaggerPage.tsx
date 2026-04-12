import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, AlertCircle } from 'lucide-react';
import { Button } from '@/components/ui/button';
import SwaggerUI from 'swagger-ui-react';
import 'swagger-ui-react/swagger-ui.css';

export default function SwaggerPage() {
  const { t } = useTranslation();
  const [state, setState] = useState<'loading' | 'loaded' | 'error'>('loading');
  const specUrl = `${window.location.origin}/v1/openapi/ironrag.openapi.yaml`;

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      <div className="flex-1 overflow-auto swagger-container">
        {state === 'error' && (
          <div className="flex flex-col items-center justify-center h-full">
            <AlertCircle className="h-8 w-8 text-destructive mb-3" />
            <h2 className="text-base font-bold">{t('swagger.failedToLoadSpec')}</h2>
            <Button variant="outline" size="sm" className="mt-3" onClick={() => window.location.reload()}>
              {t('documents.retry')}
            </Button>
          </div>
        )}
        <SwaggerUI
          url={specUrl}
          docExpansion="list"
          defaultModelsExpandDepth={-1}
          withCredentials={true}
          onComplete={() => setState('loaded')}
          onFailure={() => setState('error')}
          requestInterceptor={(req: any) => {
            req.credentials = 'include';
            return req;
          }}
        />
      </div>
    </div>
  );
}
