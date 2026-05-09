import { act } from 'react';
import type { ReactNode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { AIProvider } from '@/shared/types';

import { PresetsSection } from './PresetsSection';

vi.mock('sonner', () => ({
  toast: {
    success: vi.fn(),
    error: vi.fn(),
  },
}));

function provider(overrides: Partial<AIProvider> = {}): AIProvider {
  return {
    id: 'provider-alpha',
    displayName: 'Provider Alpha',
    kind: 'alpha',
    apiStyle: 'openai_chat',
    lifecycleState: 'active',
    apiKeyRequired: true,
    baseUrlRequired: false,
    credentialPolicy: {
      apiKeyRequired: true,
      baseUrlRequired: false,
      baseUrlMode: 'fixed',
      validationMode: 'model_list',
    },
    baseUrlPolicy: {
      allowOverride: false,
      requireHttps: true,
      allowPrivateNetwork: false,
      trimSuffixes: [],
    },
    modelDiscovery: {
      mode: 'credential',
      paths: [{ capabilityKind: 'chat', path: '/models' }],
    },
    capabilities: {},
    runtime: {},
    uiHints: {},
    modelCount: 0,
    credentialCount: 1,
    ...overrides,
  };
}

describe('PresetsSection', () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    container = document.createElement('div');
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(async () => {
    await act(async () => {
      root?.unmount();
    });
    container.remove();
    root = null;
  });

  async function render(ui: ReactNode) {
    await act(async () => {
      root?.render(ui);
    });
  }

  it('shows a visible reason when no model can be selected for a preset', async () => {
    await render(
      <PresetsSection
        selectedScope="instance"
        scopeContext={{}}
        providers={[provider()]}
        models={[]}
        presetsState={{ isLoading: false, error: null, data: [] }}
        modelById={new Map()}
        invalidateAll={vi.fn()}
      />,
    );

    const addPresetButton = Array.from(container.querySelectorAll('button'))
      .find(button => button.textContent?.includes('Add preset'));

    expect(addPresetButton).toBeDisabled();
    expect(container.textContent).toContain(
      'No selectable models are available yet. Save a reachable credential first and make sure the model is present on the configured provider.',
    );
  });
});
