import { useSearchParams } from 'react-router-dom'
import { useTranslation } from 'react-i18next'

import type { AIScopeKind } from '@/shared/types'
import { PageHeader } from '@/shared/components/layout/PageHeader'
import { PageShell } from '@/shared/components/layout/PageShell'
import AiConfigurationPanel from './AiConfigurationPanel'
import type { AiConfigSection } from '@/features/admin/model/aiConfig'

const SCOPE_VALUES = new Set<AIScopeKind>(['instance', 'workspace', 'library'])
const SECTION_VALUES = new Set<AiConfigSection>(['bindings', 'accounts', 'catalog'])

function parseScope(value: string | null): AIScopeKind | undefined {
  return SCOPE_VALUES.has(value as AIScopeKind) ? (value as AIScopeKind) : undefined
}

function parseSection(value: string | null): AiConfigSection | undefined {
  return SECTION_VALUES.has(value as AiConfigSection) ? (value as AiConfigSection) : undefined
}

/**
 * `/admin/ai` — AI configuration section. Honors deep-link query params from
 * the Libraries "Fix" link and the Library Hub "Configure AI →" button
 * (`?scope=library&lib=…&section=bindings`), and exposes the guided wizard via
 * `?wizard=1` for the first-run / cold-start path.
 */
export default function AdminAiPage() {
  const { t } = useTranslation()
  const [searchParams] = useSearchParams()
  return (
    <PageShell
      header={<PageHeader title={t('admin.nav.ai')} description={t('admin.nav.aiDesc')} />}
      bodyClassName="min-h-0 overflow-hidden p-3 sm:p-4"
    >
      <div className="flex min-h-0 w-full flex-1 flex-col animate-fade-in">
        <AiConfigurationPanel
          active
          initialScope={parseScope(searchParams.get('scope'))}
          initialSection={parseSection(searchParams.get('section'))}
          openWizardOnMount={searchParams.get('wizard') === '1'}
        />
      </div>
    </PageShell>
  )
}
