import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { AlertTriangle, ArrowRight, CheckCircle2, Database, KeyRound, Link2, Server, Wand2 } from 'lucide-react';

import { Button } from '@/shared/components/ui/button';
import { FeatureErrorBoundary } from '@/shared/components/FeatureErrorBoundary';
import { useApp } from '@/shared/contexts/app-context';
import type { AIScopeKind } from '@/shared/types';
import { AiBindingWizard } from './ai-configuration/AiBindingWizard';
import {
  purposeLabel,
  recommendAiConfigSection,
  summarizeAiReadiness,
  type AiCatalogTab,
  type AiConfigSection,
  type AiReadinessSummary,
} from '@/features/admin/model/aiConfig';
import { AccountsSection } from './ai-configuration/AccountsSection';
import { BindingsSection } from './ai-configuration/BindingsSection';
import { ModelsSection } from './ai-configuration/ModelsSection';
import { ProvidersSection } from './ai-configuration/ProvidersSection';
import { ScopePicker } from './ai-configuration/ScopePicker';
import { useAiConfigQueries } from './ai-configuration/useAiConfigQueries';

type AiConfigurationPanelProps = {
  active: boolean;
  /**
   * Deep-link entry points (ADM-04): the Libraries readiness "Fix" link and the
   * Library Hub "Configure AI →" button route here with a scope + section so the
   * panel opens directly on the binding the operator needs to fix.
   */
  initialScope?: AIScopeKind | undefined;
  initialSection?: AiConfigSection | undefined;
  /** Open the guided binding wizard on mount (first-run / cold-start path). */
  openWizardOnMount?: boolean;
};

const SETUP_SECTIONS = ['bindings', 'accounts'] satisfies AiConfigSection[];
const SECTION_ICONS = {
  bindings: Link2,
  accounts: KeyRound,
  catalog: Database,
} satisfies Record<AiConfigSection, typeof Link2>;

function sectionLabel(section: AiConfigSection, t: TFunction) {
  if (section === 'bindings') return t('admin.aiPanel.sections.bindingsTitle');
  if (section === 'accounts') return t('admin.accounts');
  return t('admin.aiPanel.navigation.catalogLink');
}

function sectionMetric(section: AiConfigSection, summary: AiReadinessSummary) {
  if (section === 'bindings') return `${summary.executableEffectiveBindings}/${summary.totalPurposes}`;
  if (section === 'accounts') return String(summary.localAccountCount);
  return String(summary.visibleModelCount);
}

function sectionNeedsAttention(section: AiConfigSection, summary: AiReadinessSummary) {
  if (section === 'bindings') return summary.missingPurposes.length > 0;
  if (section === 'accounts') return summary.activeAccountCount === 0;
  return false;
}

function readinessTone(summary: AiReadinessSummary) {
  if (summary.activeAccountCount === 0) {
    return 'warning';
  }
  return summary.missingPurposes.length === 0 ? 'ready' : 'warning';
}

type ReadinessPanelProps = {
  activeSection: AiConfigSection;
  summary: AiReadinessSummary;
  onOpenRecommendedSection: (section: AiConfigSection) => void;
  t: TFunction;
};

function AiReadinessPanel({ activeSection, summary, onOpenRecommendedSection, t }: ReadinessPanelProps) {
  const recommendedSection = recommendAiConfigSection(summary);
  const tone = readinessTone(summary);
  const showAction = recommendedSection !== activeSection;
  const StatusIcon = tone === 'ready' ? CheckCircle2 : AlertTriangle;
  const statusTitle =
    tone === 'ready'
      ? t('admin.aiPanel.readiness.readyTitle')
      : summary.activeAccountCount === 0
        ? t('admin.aiPanel.readiness.missingAccountsTitle')
        : t('admin.aiPanel.readiness.missingBindingsTitle', { count: summary.missingPurposes.length });
  const statusDetail =
    tone === 'ready'
      ? t('admin.aiPanel.readiness.readyDetail')
      : summary.activeAccountCount === 0
        ? t('admin.aiPanel.readiness.missingAccountsDetail')
        : t('admin.aiPanel.readiness.missingBindingsDetail', {
            purposes: summary.missingPurposes
              .slice(0, 3)
              .map(purpose => purposeLabel(purpose, t))
              .join(', '),
          });

  return (
    <div className={`rounded-xl border p-3 shadow-soft sm:flex sm:items-center sm:justify-between sm:gap-4 ${
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
            <span className={`rounded-full px-2 py-0.5 text-2xs font-bold ${
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
  t: TFunction;
};

function AiSectionNavigation({ activeSection, summary, onSelectSection, t }: SectionNavigationProps) {
  const renderSectionButton = (section: AiConfigSection) => {
    const Icon = SECTION_ICONS[section];
    const active = activeSection === section;
    const needsAttention = sectionNeedsAttention(section, summary);
    const isBindings = section === 'bindings';

    return (
      <button
        key={section}
        type="button"
        aria-current={active ? 'page' : undefined}
        onClick={() => onSelectSection(section)}
        className={`flex min-h-11 min-w-0 items-center gap-2 rounded-lg border px-3 py-2 text-left transition-colors ${
          active
            ? 'border-primary/30 bg-accent/10 text-foreground ring-1 ring-primary/30 shadow-soft'
            : 'border-border/70 bg-card text-foreground hover:border-primary/20 hover:bg-muted/60'
        }`}
      >
        <span className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-md ${
          active ? 'bg-primary/10 text-primary' : 'bg-muted text-muted-foreground'
        }`}>
          <Icon className="h-4 w-4" />
        </span>
        <span className="min-w-0 flex-1 truncate text-sm font-bold">{sectionLabel(section, t)}</span>
        <span className={`shrink-0 tabular-nums ${
          isBindings
            ? `text-xs font-semibold ${needsAttention ? 'text-status-warning' : 'text-muted-foreground'}`
            : 'text-xs text-muted-foreground'
        }`}>
          {sectionMetric(section, summary)}
        </span>
      </button>
    );
  };

  return (
    <nav
      aria-label={t('admin.aiPanel.navigation.label')}
      className="workbench-surface space-y-3 p-3"
    >
      <div className="space-y-2">
        <div className="section-label text-muted-foreground">
          {t('admin.aiPanel.navigation.setupGroup')}
        </div>
        <div className="grid gap-2 sm:grid-cols-2">
          {SETUP_SECTIONS.map(renderSectionButton)}
        </div>
      </div>
      <button
        type="button"
        aria-current={activeSection === 'catalog' ? 'page' : undefined}
        onClick={() => onSelectSection('catalog')}
        className={`inline-flex items-center gap-1.5 text-sm font-semibold transition-colors ${
          activeSection === 'catalog' ? 'text-primary' : 'text-muted-foreground hover:text-primary'
        }`}
      >
        {t('admin.aiPanel.navigation.catalogLink')}
        <ArrowRight className="h-3.5 w-3.5" />
      </button>
    </nav>
  );
}

type CatalogSectionProps = {
  catalogTab: AiCatalogTab;
  onCatalogTabChange: (tab: AiCatalogTab) => void;
  aiConfig: ReturnType<typeof useAiConfigQueries>;
  activeWorkspaceId: string | undefined;
  t: TFunction;
};

function CatalogSection({ catalogTab, onCatalogTabChange, aiConfig, activeWorkspaceId, t }: CatalogSectionProps) {
  const tabs: Array<{ key: AiCatalogTab; icon: typeof Server; label: string }> = [
    { key: 'providers', icon: Server, label: t('admin.providers') },
    { key: 'models', icon: Database, label: t('admin.aiPanel.metrics.visibleModels') },
  ];
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3">
      <div className="flex gap-2">
        {tabs.map(tab => {
          const Icon = tab.icon;
          const active = catalogTab === tab.key;
          return (
            <button
              key={tab.key}
              type="button"
              aria-current={active ? 'page' : undefined}
              onClick={() => onCatalogTabChange(tab.key)}
              className={`inline-flex items-center gap-1.5 rounded-lg border px-3 py-1.5 text-sm font-semibold transition-colors ${
                active
                  ? 'border-primary/30 bg-accent/10 text-foreground ring-1 ring-primary/30'
                  : 'border-border/70 bg-card text-muted-foreground hover:border-primary/20 hover:text-foreground'
              }`}
            >
              <Icon className="h-3.5 w-3.5" />
              {tab.label}
            </button>
          );
        })}
      </div>
      <div className="flex min-h-0 flex-1 flex-col">
        {catalogTab === 'providers' ? (
          <ProvidersSection
            providersState={aiConfig.providersState}
            models={aiConfig.models}
            accounts={aiConfig.localAccounts}
            invalidateAll={aiConfig.invalidateAll}
          />
        ) : (
          <ModelsSection
            modelsState={aiConfig.modelsState}
            providers={aiConfig.providers}
            prices={aiConfig.prices}
            activeWorkspaceId={activeWorkspaceId}
            invalidateAll={aiConfig.invalidateAll}
          />
        )}
      </div>
    </div>
  );
}

export default function AiConfigurationPanel({
  active,
  initialScope,
  initialSection,
  openWizardOnMount,
}: AiConfigurationPanelProps) {
  const { t } = useTranslation();
  const { activeWorkspace, activeLibrary } = useApp();
  const [selectedScope, setSelectedScope] = useState<AIScopeKind>(initialScope ?? 'instance');
  const [activeSection, setActiveSection] = useState<AiConfigSection>(initialSection ?? 'bindings');
  const [catalogTab, setCatalogTab] = useState<AiCatalogTab>('providers');
  const [accountAddRequest, setAccountAddRequest] = useState(0);
  const [wizardOpen, setWizardOpen] = useState(false);
  // When a deep-link arrives, opt out of the panel's own scope auto-selection
  // so we honor the requested scope (e.g. a library Fix link) instead.
  const autoSelectedScopeRef = useRef(Boolean(initialScope));

  // Open the guided wizard once on cold-start / first-run entry.
  const wizardAutoOpenedRef = useRef(false);
  useEffect(() => {
    if (openWizardOnMount && !wizardAutoOpenedRef.current) {
      wizardAutoOpenedRef.current = true;
      setWizardOpen(true);
    }
  }, [openWizardOnMount]);
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
      availableAccounts: aiConfig.availableAccounts,
      localAccounts: aiConfig.localAccounts,
      bindingsForScope: aiConfig.bindingsForScope,
      instanceBindings: aiConfig.instanceBindings,
      workspaceBindings: aiConfig.workspaceBindings,
      models: aiConfig.models,
      providers: aiConfig.providers,
      priceRuleCount: aiConfig.priceRuleCount,
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
      aiConfig.instanceBindings.length > 0 || aiConfig.localAccounts.length > 0;
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
    if (section === 'accounts') {
      setAccountAddRequest(request => request + 1);
    }
  };

  const section = activeSection === 'bindings' ? (
    <BindingsSection
      selectedScope={selectedScope}
      scopeContext={aiConfig.scopeContext}
      bindingsState={aiConfig.bindingsState}
      availableAccounts={aiConfig.availableAccounts}
      localAccounts={aiConfig.localAccounts}
      models={aiConfig.models}
      prices={aiConfig.prices}
      bindingsForScope={aiConfig.bindingsForScope}
      instanceBindings={aiConfig.instanceBindings}
      workspaceBindings={aiConfig.workspaceBindings}
      modelById={aiConfig.modelById}
      invalidateAll={aiConfig.invalidateAll}
    />
  ) : activeSection === 'accounts' ? (
    <AccountsSection
      selectedScope={selectedScope}
      scopeContext={aiConfig.scopeContext}
      providers={aiConfig.providers}
      accountsState={aiConfig.accountsState}
      invalidateAll={aiConfig.invalidateAll}
      openAddRequest={accountAddRequest}
    />
  ) : (
    <CatalogSection
      catalogTab={catalogTab}
      onCatalogTabChange={setCatalogTab}
      aiConfig={aiConfig}
      activeWorkspaceId={activeWorkspace?.id}
      t={t}
    />
  );

  if (!active) {
    return null;
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-auto">
      <div className="workbench-surface p-3">
        <div className="flex flex-col gap-3 xl:flex-row xl:items-center xl:justify-between">
          <Button type="button" size="sm" onClick={() => setWizardOpen(true)}>
            <Wand2 className="mr-1.5 h-3.5 w-3.5" />
            {t('admin.aiWizard.launch')}
          </Button>
          <ScopePicker selectedScope={selectedScope} activeWorkspaceName={activeWorkspace?.name} activeLibraryName={activeLibrary?.name} onScopeChange={setSelectedScope} />
        </div>
      </div>
      <AiBindingWizard
        open={wizardOpen}
        onOpenChange={setWizardOpen}
        selectedScope={selectedScope}
        scopeContext={aiConfig.scopeContext}
        activeWorkspaceName={activeWorkspace?.name}
        activeLibraryName={activeLibrary?.name}
        onScopeChange={setSelectedScope}
        availableAccounts={aiConfig.availableAccounts}
        providers={aiConfig.providers}
        models={aiConfig.models}
        prices={aiConfig.prices}
        bindingsForScope={aiConfig.bindingsForScope}
        instanceBindings={aiConfig.instanceBindings}
        workspaceBindings={aiConfig.workspaceBindings}
        invalidateAll={aiConfig.invalidateAll}
      />
      <AiReadinessPanel
        activeSection={activeSection}
        summary={readinessSummary}
        t={t}
        onOpenRecommendedSection={openRecommendedSection}
      />
      {readinessSummary.missingOptionalPurposes.length > 0 && (
        <div className="flex items-start gap-3 rounded-xl border border-status-warning/25 bg-status-warning/5 p-3 shadow-soft">
          <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-status-warning" />
          <div className="min-w-0">
            <p className="text-sm font-bold text-status-warning">
              {t('admin.aiPanel.optionalBindingsMissingTitle')}
            </p>
            <p className="mt-1 text-sm text-muted-foreground">
              {t('admin.aiPanel.optionalBindingsMissingDetail', {
                purposes: readinessSummary.missingOptionalPurposes
                  .map(p => purposeLabel(p, t))
                  .join(', '),
              })}
            </p>
          </div>
        </div>
      )}
      <div className="flex min-h-0 flex-1 flex-col gap-3">
        <AiSectionNavigation
          activeSection={activeSection}
          summary={readinessSummary}
          t={t}
          onSelectSection={setActiveSection}
        />
        <div className="flex min-w-0 flex-col lg:min-h-0 lg:overflow-auto">
          <FeatureErrorBoundary feature={t('admin.aiPanel.featureName')}>{section}</FeatureErrorBoundary>
        </div>
      </div>
    </div>
  );
}
