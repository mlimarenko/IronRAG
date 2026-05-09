import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, Brain, CheckCircle2, Database, KeyRound, Link2, Server } from 'lucide-react';

import { Button } from '@/shared/components/ui/button';
import { FeatureErrorBoundary } from '@/shared/components/FeatureErrorBoundary';
import { useApp } from '@/shared/contexts/app-context';
import type { AIScopeKind } from '@/shared/types';
import {
  purposeLabel,
  recommendAiConfigSection,
  summarizeAiReadiness,
  type AiConfigSection,
  type AiReadinessSummary,
} from '@/features/admin/model/aiConfig';
import { BindingsSection } from './ai-configuration/BindingsSection';
import { CredentialsSection } from './ai-configuration/CredentialsSection';
import { ModelsSection } from './ai-configuration/ModelsSection';
import { PresetsSection } from './ai-configuration/PresetsSection';
import { ProvidersSection } from './ai-configuration/ProvidersSection';
import { ScopePicker } from './ai-configuration/ScopePicker';
import { useAiConfigQueries } from './ai-configuration/useAiConfigQueries';

type AiConfigurationPanelProps = {
  active: boolean;
};

const SETUP_SECTIONS = ['bindings', 'credentials', 'presets'] satisfies AiConfigSection[];
const CATALOG_SECTIONS = ['providers', 'models'] satisfies AiConfigSection[];
const SECTION_ICONS = {
  bindings: Link2,
  credentials: KeyRound,
  presets: Brain,
  providers: Server,
  models: Database,
} satisfies Record<AiConfigSection, typeof Link2>;

function sectionLabel(section: AiConfigSection, t: (key: string) => string) {
  if (section === 'bindings') return t('admin.aiPanel.sections.bindingsTitle');
  if (section === 'credentials') return t('admin.credentials');
  if (section === 'presets') return t('admin.modelPresets');
  if (section === 'providers') return t('admin.providers');
  return t('admin.aiPanel.metrics.visibleModels');
}

function sectionMetric(section: AiConfigSection, summary: AiReadinessSummary) {
  if (section === 'bindings') return `${summary.executableEffectiveBindings}/${summary.totalPurposes}`;
  if (section === 'credentials') return String(summary.localCredentialCount);
  if (section === 'presets') return String(summary.localPresetCount);
  if (section === 'providers') return String(summary.providerCatalogCount);
  return String(summary.visibleModelCount);
}

function sectionNeedsAttention(section: AiConfigSection, summary: AiReadinessSummary) {
  if (section === 'bindings') return summary.missingPurposes.length > 0;
  if (section === 'credentials') return summary.activeCredentialCount === 0;
  if (section === 'presets') return summary.usablePresetCount === 0;
  return false;
}

function readinessTone(summary: AiReadinessSummary) {
  if (summary.activeCredentialCount === 0 || summary.usablePresetCount === 0) {
    return 'warning';
  }
  return summary.missingPurposes.length === 0 ? 'ready' : 'warning';
}

type ReadinessPanelProps = {
  activeSection: AiConfigSection;
  summary: AiReadinessSummary;
  onOpenRecommendedSection: (section: AiConfigSection) => void;
  t: (key: string, options?: Record<string, unknown>) => string;
};

function AiReadinessPanel({ activeSection, summary, onOpenRecommendedSection, t }: ReadinessPanelProps) {
  const recommendedSection = recommendAiConfigSection(summary);
  const tone = readinessTone(summary);
  const showAction = recommendedSection !== activeSection;
  const StatusIcon = tone === 'ready' ? CheckCircle2 : AlertTriangle;
  const statusTitle =
    tone === 'ready'
      ? t('admin.aiPanel.readiness.readyTitle')
      : summary.activeCredentialCount === 0
        ? t('admin.aiPanel.readiness.missingCredentialsTitle')
        : summary.usablePresetCount === 0
          ? t('admin.aiPanel.readiness.missingPresetsTitle')
          : t('admin.aiPanel.readiness.missingBindingsTitle', { count: summary.missingPurposes.length });
  const statusDetail =
    tone === 'ready'
      ? t('admin.aiPanel.readiness.readyDetail')
      : summary.activeCredentialCount === 0
        ? t('admin.aiPanel.readiness.missingCredentialsDetail')
        : summary.usablePresetCount === 0
          ? t('admin.aiPanel.readiness.missingPresetsDetail')
          : t('admin.aiPanel.readiness.missingBindingsDetail', {
            purposes: summary.missingPurposes
              .slice(0, 3)
              .map(purpose => purposeLabel(purpose, t))
              .join(', '),
          });

  return (
    <div className={`rounded-md border p-3 sm:flex sm:items-center sm:justify-between sm:gap-4 ${
      tone === 'ready' ? 'border-status-ready/20 bg-status-ready/5' : 'border-status-warning/25 bg-status-warning/5'
    }`}>
      <div className="flex min-w-0 items-start gap-3">
        <div className={`mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-md ${
          tone === 'ready' ? 'bg-status-ready-bg text-status-ready' : 'bg-status-warning-bg text-status-warning'
        }`}>
          <StatusIcon className="h-4 w-4" />
        </div>
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="text-sm font-bold tracking-tight">{statusTitle}</h3>
            <span className={`rounded-full px-2 py-0.5 text-[11px] font-bold ${
              tone === 'ready' ? 'bg-status-ready-bg text-status-ready' : 'bg-status-warning-bg text-status-warning'
            }`}>
              {summary.executableEffectiveBindings}/{summary.totalPurposes}
            </span>
          </div>
          <p className="mt-1 max-w-4xl text-sm leading-5 text-muted-foreground">{statusDetail}</p>
        </div>
      </div>
      {showAction && (
        <Button
          type="button"
          size="sm"
          variant={tone === 'ready' ? 'outline' : 'default'}
          className="mt-3 w-full justify-center sm:mt-0 sm:w-auto"
          onClick={() => onOpenRecommendedSection(recommendedSection)}
        >
          {t(`admin.aiPanel.readiness.actions.${recommendedSection}`)}
        </Button>
      )}
    </div>
  );
}

type SectionNavigationProps = {
  activeSection: AiConfigSection;
  summary: AiReadinessSummary;
  onSelectSection: (section: AiConfigSection) => void;
  t: (key: string, options?: Record<string, unknown>) => string;
};

function AiSectionNavigation({ activeSection, summary, onSelectSection, t }: SectionNavigationProps) {
  const renderSectionButton = (section: AiConfigSection) => {
    const Icon = SECTION_ICONS[section];
    const active = activeSection === section;
    const needsAttention = sectionNeedsAttention(section, summary);

    return (
      <button
        key={section}
        type="button"
        aria-current={active ? 'page' : undefined}
        onClick={() => onSelectSection(section)}
        className={`flex min-h-[4.5rem] min-w-0 items-start gap-3 rounded-md border p-3 text-left transition sm:min-h-[4rem] lg:min-h-0 ${
          active
            ? 'border-primary bg-primary text-primary-foreground shadow-sm'
            : 'border-border/70 bg-background hover:border-primary/30 hover:bg-muted/60'
        }`}
      >
        <span className={`mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-md ${
          active ? 'bg-primary-foreground/15 text-primary-foreground' : 'bg-muted text-muted-foreground'
        }`}>
          <Icon className="h-4 w-4" />
        </span>
        <span className="min-w-0 flex-1">
          <span className="flex min-w-0 items-center justify-between gap-2">
            <span className="truncate text-sm font-bold">{sectionLabel(section, t)}</span>
            <span className={`shrink-0 rounded-full px-2 py-0.5 text-[11px] font-bold ${
              active
                ? 'bg-primary-foreground/15 text-primary-foreground'
                : needsAttention
                  ? 'bg-status-warning-bg text-status-warning'
                  : 'bg-muted text-muted-foreground'
            }`}>
              {sectionMetric(section, summary)}
            </span>
          </span>
          <span className={`mt-1 block text-xs leading-4 ${
            active ? 'text-primary-foreground/80' : 'text-muted-foreground'
          }`}>
            {t(`admin.aiPanel.navigation.descriptions.${section}`)}
          </span>
        </span>
      </button>
    );
  };

  return (
    <nav
      aria-label={t('admin.aiPanel.navigation.label')}
      className="rounded-md border border-border/70 bg-card p-2 lg:sticky lg:top-3"
    >
      <div className="space-y-3">
        <div>
          <div className="px-1 pb-1 text-[11px] font-bold uppercase tracking-wide text-muted-foreground">
            {t('admin.aiPanel.navigation.setupGroup')}
          </div>
          <div className="grid gap-2 sm:grid-cols-3 lg:grid-cols-1">
            {SETUP_SECTIONS.map(renderSectionButton)}
          </div>
        </div>
        <div>
          <div className="px-1 pb-1 text-[11px] font-bold uppercase tracking-wide text-muted-foreground">
            {t('admin.aiPanel.navigation.catalogGroup')}
          </div>
          <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-1">
            {CATALOG_SECTIONS.map(renderSectionButton)}
          </div>
        </div>
      </div>
    </nav>
  );
}

export default function AiConfigurationPanel({ active }: AiConfigurationPanelProps) {
  const { t } = useTranslation();
  const { activeWorkspace, activeLibrary } = useApp();
  const [selectedScope, setSelectedScope] = useState<AIScopeKind>('instance');
  const [activeSection, setActiveSection] = useState<AiConfigSection>('bindings');
  const [credentialAddRequest, setCredentialAddRequest] = useState(0);
  const [presetAddRequest, setPresetAddRequest] = useState(0);
  const autoSelectedScopeRef = useRef(false);
  const aiConfig = useAiConfigQueries({
    active,
    activeSection,
    selectedScope,
    workspaceId: activeWorkspace?.id,
    libraryId: activeLibrary?.id,
  });
  const readinessSummary = useMemo(
    () => summarizeAiReadiness({
      selectedScope,
      availableCredentials: aiConfig.availableCredentials,
      localCredentials: aiConfig.localCredentials,
      availablePresets: aiConfig.availablePresets,
      localPresets: aiConfig.localPresets,
      bindingsForScope: aiConfig.bindingsForScope,
      instanceBindings: aiConfig.instanceBindings,
      workspaceBindings: aiConfig.workspaceBindings,
      models: aiConfig.models,
      providers: aiConfig.providers,
    }),
    [aiConfig, selectedScope],
  );

  useEffect(() => {
    const nextScope =
      selectedScope === 'library' && !activeLibrary
        ? activeWorkspace ? 'workspace' : 'instance'
        : selectedScope === 'workspace' && !activeWorkspace
          ? 'instance'
          : null;
    if (!nextScope) return;
    let cancelled = false;
    queueMicrotask(() => {
      if (!cancelled) setSelectedScope(nextScope);
    });
    return () => {
      cancelled = true;
    };
  }, [activeLibrary, activeWorkspace, selectedScope]);

  useEffect(() => {
    if (activeSection !== 'bindings' || aiConfig.bindingsState.isLoading || selectedScope !== 'instance' || autoSelectedScopeRef.current) {
      return;
    }
    const hasInstanceBaseline =
      aiConfig.instanceBindings.length > 0 || aiConfig.localCredentials.length > 0 || aiConfig.localPresets.length > 0;
    if (hasInstanceBaseline) {
      autoSelectedScopeRef.current = true;
      return;
    }
    const nextScope =
      activeLibrary && aiConfig.libraryBindings.length > 0
        ? 'library'
        : activeWorkspace && aiConfig.workspaceBindings.length > 0
          ? 'workspace'
          : null;
    if (!nextScope) return;
    autoSelectedScopeRef.current = true;
    let cancelled = false;
    queueMicrotask(() => {
      if (!cancelled) setSelectedScope(nextScope);
    });
    return () => {
      cancelled = true;
    };
  }, [activeLibrary, activeSection, activeWorkspace, aiConfig, selectedScope]);

  const openRecommendedSection = (section: AiConfigSection) => {
    setActiveSection(section);
    if (section === 'credentials') {
      setCredentialAddRequest(request => request + 1);
    }
    if (section === 'presets') {
      setPresetAddRequest(request => request + 1);
    }
  };

  const section = activeSection === 'bindings' ? (
    <BindingsSection selectedScope={selectedScope} scopeContext={aiConfig.scopeContext} bindingsState={aiConfig.bindingsState} availableCredentials={aiConfig.availableCredentials} availablePresets={aiConfig.availablePresets} localCredentials={aiConfig.localCredentials} localPresets={aiConfig.localPresets} bindingsForScope={aiConfig.bindingsForScope} instanceBindings={aiConfig.instanceBindings} workspaceBindings={aiConfig.workspaceBindings} modelById={aiConfig.modelById} invalidateAll={aiConfig.invalidateAll} />
  ) : activeSection === 'credentials' ? (
    <CredentialsSection selectedScope={selectedScope} scopeContext={aiConfig.scopeContext} providers={aiConfig.providers} credentialsState={aiConfig.credentialsState} invalidateAll={aiConfig.invalidateAll} openAddRequest={credentialAddRequest} />
  ) : activeSection === 'presets' ? (
    <PresetsSection selectedScope={selectedScope} scopeContext={aiConfig.scopeContext} providers={aiConfig.providers} models={aiConfig.models} presetsState={aiConfig.presetsState} modelById={aiConfig.modelById} invalidateAll={aiConfig.invalidateAll} openAddRequest={presetAddRequest} />
  ) : activeSection === 'providers' ? (
    <ProvidersSection providersState={aiConfig.providersState} />
  ) : (
    <ModelsSection modelsState={aiConfig.modelsState} providers={aiConfig.providers} />
  );

  if (!active) {
    return null;
  }

  return (
    <div className="flex flex-1 min-h-0 flex-col gap-4">
      <div className="flex flex-col gap-3 xl:flex-row xl:items-center xl:justify-between">
        <h2 className="text-base font-bold tracking-tight">{t('admin.aiPanel.title')}</h2>
        <ScopePicker selectedScope={selectedScope} activeWorkspaceName={activeWorkspace?.name} activeLibraryName={activeLibrary?.name} onScopeChange={setSelectedScope} />
      </div>
      <AiReadinessPanel
        activeSection={activeSection}
        summary={readinessSummary}
        t={t}
        onOpenRecommendedSection={openRecommendedSection}
      />
      <div className="grid flex-1 min-h-0 gap-4 lg:grid-cols-[18rem_minmax(0,1fr)] lg:items-stretch">
        <AiSectionNavigation
          activeSection={activeSection}
          summary={readinessSummary}
          t={t}
          onSelectSection={setActiveSection}
        />
        <div className="flex min-w-0 min-h-0 flex-col">
          <FeatureErrorBoundary feature={t('admin.aiPanel.featureName')}>{section}</FeatureErrorBoundary>
        </div>
      </div>
    </div>
  );
}
