import { useSearchParams } from 'react-router-dom';

import type { AIScopeKind } from '@/shared/types';
import AiConfigurationPanel from './AiConfigurationPanel';
import type { AiConfigSection } from '@/features/admin/model/aiConfig';

const SCOPE_VALUES: AIScopeKind[] = ['instance', 'workspace', 'library'];
const SECTION_VALUES: AiConfigSection[] = [
  'bindings',
  'credentials',
  'presets',
  'providers',
  'models',
  'pricing',
];

function parseScope(value: string | null): AIScopeKind | undefined {
  return SCOPE_VALUES.includes(value as AIScopeKind) ? (value as AIScopeKind) : undefined;
}

function parseSection(value: string | null): AiConfigSection | undefined {
  return SECTION_VALUES.includes(value as AiConfigSection) ? (value as AiConfigSection) : undefined;
}

/**
 * `/admin/ai` — AI configuration section. Honors deep-link query params from
 * the Libraries "Fix" link and the Library Hub "Configure AI →" button
 * (`?scope=library&lib=…&section=bindings`), and exposes the guided wizard via
 * `?wizard=1` for the first-run / cold-start path.
 */
export default function AdminAiPage() {
  const [searchParams] = useSearchParams();
  return (
    <div className="flex flex-1 min-h-0 flex-col p-6">
      <AiConfigurationPanel
        active
        initialScope={parseScope(searchParams.get('scope'))}
        initialSection={parseSection(searchParams.get('section'))}
        openWizardOnMount={searchParams.get('wizard') === '1'}
      />
    </div>
  );
}
