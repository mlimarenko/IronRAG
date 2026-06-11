import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ArrowRight, Check, KeyRound, Link2, Sparkles } from 'lucide-react';

import { Button } from '@/shared/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/shared/components/ui/dialog';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/shared/components/ui/select';
import type { AIScopeKind } from '@/shared/types';
import {
  purposeLabel,
  REQUIRED_RUNTIME_PURPOSE_ORDER,
  type AiConfigSection,
  type AiReadinessSummary,
} from '@/features/admin/model/aiConfig';

type WizardStep = {
  key: 'credential' | 'preset' | 'binding';
  section: AiConfigSection;
  icon: typeof KeyRound;
  done: boolean;
};

type AiBindingWizardProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  summary: AiReadinessSummary;
  selectedScope: AIScopeKind;
  activeWorkspaceName?: string | undefined;
  activeLibraryName?: string | undefined;
  onScopeChange: (scope: AIScopeKind) => void;
  /** Jump the panel to a section, opening its add-form where applicable. */
  onOpenSection: (section: AiConfigSection) => void;
};

/**
 * Guided AI-binding wizard (ADM-01). The cold-start path used to be ~13–15
 * clicks spread across three disconnected sub-sections (Credentials → Presets
 * → Bindings) with a banner that only ever names one next step. This sheet
 * presents the same three steps as one numbered checklist for a chosen
 * purpose + scope: each step shows whether it's already satisfied and routes
 * straight to the exact form, so the operator is never left guessing what's
 * next. It orchestrates the existing, well-tested mutation forms rather than
 * duplicating them.
 */
export function AiBindingWizard({
  open,
  onOpenChange,
  summary,
  selectedScope,
  activeWorkspaceName,
  activeLibraryName,
  onScopeChange,
  onOpenSection,
}: AiBindingWizardProps) {
  const { t } = useTranslation();
  const [purpose, setPurpose] = useState<string>(REQUIRED_RUNTIME_PURPOSE_ORDER[0]);

  const scopeOptions = useMemo(
    () =>
      [
        { kind: 'instance' as const, label: t('admin.aiPanel.scopeCards.instanceTitle'), enabled: true },
        {
          kind: 'workspace' as const,
          label: activeWorkspaceName ?? t('admin.aiPanel.scopeCards.workspaceTitle'),
          enabled: Boolean(activeWorkspaceName),
        },
        {
          kind: 'library' as const,
          label: activeLibraryName ?? t('admin.aiPanel.scopeCards.libraryTitle'),
          enabled: Boolean(activeLibraryName),
        },
      ].filter((option) => option.enabled),
    [activeWorkspaceName, activeLibraryName, t],
  );

  const credentialDone = summary.activeCredentialCount > 0;
  const presetDone = summary.usablePresetCount > 0;
  const bindingDone = !summary.missingPurposes.includes(purpose as never);

  const steps: WizardStep[] = [
    { key: 'credential', section: 'credentials', icon: KeyRound, done: credentialDone },
    { key: 'preset', section: 'presets', icon: Sparkles, done: presetDone },
    { key: 'binding', section: 'bindings', icon: Link2, done: bindingDone },
  ];

  const completedCount = steps.filter((step) => step.done).length;
  const nextStep = steps.find((step) => !step.done);

  const handleOpenSection = (section: AiConfigSection) => {
    onOpenSection(section);
    onOpenChange(false);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Sparkles className="h-4 w-4 text-primary" />
            {t('admin.aiWizard.title')}
          </DialogTitle>
          <DialogDescription>{t('admin.aiWizard.description')}</DialogDescription>
        </DialogHeader>

        <div className="space-y-5">
          {/* Purpose + scope selection for the binding being set up. */}
          <div className="grid gap-3 sm:grid-cols-2">
            <div>
              <label className="section-label mb-1.5 block">{t('admin.aiWizard.purposeLabel')}</label>
              <Select value={purpose} onValueChange={setPurpose}>
                <SelectTrigger className="h-9 text-sm">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {REQUIRED_RUNTIME_PURPOSE_ORDER.map((p) => (
                    <SelectItem key={p} value={p}>
                      {purposeLabel(p, t)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div>
              <label className="section-label mb-1.5 block">{t('admin.aiWizard.scopeLabel')}</label>
              <Select value={selectedScope} onValueChange={(value) => onScopeChange(value as AIScopeKind)}>
                <SelectTrigger className="h-9 text-sm">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {scopeOptions.map((option) => (
                    <SelectItem key={option.kind} value={option.kind}>
                      {option.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          </div>

          {/* Step checklist */}
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <span className="section-label">{t('admin.aiWizard.checklistTitle')}</span>
              <span className="text-xs font-bold text-muted-foreground">
                {t('admin.aiWizard.progress', { done: completedCount, total: steps.length })}
              </span>
            </div>
            {steps.map((step, index) => {
              const Icon = step.icon;
              const isNext = nextStep?.key === step.key;
              return (
                <button
                  key={step.key}
                  type="button"
                  onClick={() => handleOpenSection(step.section)}
                  className={`flex w-full items-center gap-3 rounded-xl border p-3 text-left transition-colors ${
                    step.done
                      ? 'border-status-ready/25 bg-status-ready/5'
                      : isNext
                        ? 'border-primary bg-primary/5 hover:bg-primary/10'
                        : 'border-border/70 hover:bg-accent/40'
                  }`}
                >
                  <span
                    className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-lg ${
                      step.done
                        ? 'bg-status-ready-bg text-status-ready'
                        : 'bg-muted text-muted-foreground'
                    }`}
                  >
                    {step.done ? <Check className="h-4 w-4" /> : <Icon className="h-4 w-4" />}
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="flex items-center gap-2 text-sm font-bold">
                      {t('admin.aiWizard.stepNumber', { number: index + 1 })}{' '}
                      {t(`admin.aiWizard.steps.${step.key}.title`)}
                    </span>
                    <span className="mt-0.5 block text-xs text-muted-foreground">
                      {step.done
                        ? t('admin.aiWizard.stepDone')
                        : t(`admin.aiWizard.steps.${step.key}.desc`)}
                    </span>
                  </span>
                  {!step.done && <ArrowRight className="h-4 w-4 shrink-0 text-muted-foreground" />}
                </button>
              );
            })}
          </div>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t('admin.cancel')}
          </Button>
          {nextStep ? (
            <Button onClick={() => handleOpenSection(nextStep.section)}>
              {t('admin.aiWizard.continue')}
              <ArrowRight className="ml-1.5 h-3.5 w-3.5" />
            </Button>
          ) : (
            <Button onClick={() => onOpenChange(false)}>
              <Check className="mr-1.5 h-3.5 w-3.5" />
              {t('admin.aiWizard.allDone')}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
