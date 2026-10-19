import { AlertTriangle, ArrowRight, CheckCircle2, CircleAlert } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { useApp } from '@/shared/contexts/app-context'
import { formatDate } from '@/shared/format'
import { modelAvailabilityStateLabel, validationStateLabel } from '@/shared/lib/enumLabels'
import type { AIAccount } from '@/shared/types'
import { Badge, Button } from '@/shared/ui'
import type { AdminModelOption } from '../model/aiAdapter'
import { bindingRequestTuning } from './bindingForm'
import type { BindingResolution } from '../model/aiConfig'
import { useAiAdminMutations, useBindingValidation, useBindingValidations } from './api'

export function ValidationHistory({ bindingId }: Readonly<{ bindingId: string }>) {
  const { t } = useTranslation()
  const { locale } = useApp()
  const mutations = useAiAdminMutations()
  const history = useBindingValidations(bindingId)
  const [activeId, setActiveId] = useState<string | null>(null)
  const active = useBindingValidation(bindingId, activeId)
  const refetchHistory = history.refetch
  useEffect(() => {
    if (active.data && active.data.validationState !== 'pending') void refetchHistory()
  }, [active.data, refetchHistory])
  const validate = async () => {
    const result = await mutations.bindingValidate.mutateAsync(bindingId)
    setActiveId(result.id)
  }
  return (
    <div className="validation-history">
      <div className="validation-history__heading">
        <strong>{t('admin.validationHistory')}</strong>
        <Button
          size="sm"
          type="button"
          onClick={() => void validate()}
          disabled={mutations.bindingValidate.isPending}
        >
          {t('admin.validate')}
        </Button>
      </div>
      {active.data?.validationState === 'pending' && (
        <div className="aicfg-validation-entry">
          <CircleAlert size={15} />
          <span>{t('admin.aiPanel.validation.pending')}</span>
          <time dateTime={active.data.checkedAt} title={formatDate(active.data.checkedAt, locale)}>
            {formatDate(active.data.checkedAt, locale, { dateStyle: 'short', timeStyle: 'short' })}
          </time>
        </div>
      )}
      {(history.data ?? []).map((attempt) => (
        <div className="aicfg-validation-entry" key={attempt.id}>
          {attempt.validationState === 'succeeded' ? (
            <CheckCircle2 size={15} />
          ) : (
            <CircleAlert size={15} />
          )}
          <div>
            <strong>{validationStateLabel(attempt.validationState, t)}</strong>
            {attempt.failureCode && <code>{attempt.failureCode}</code>}
            {attempt.message && <span>{attempt.message}</span>}
          </div>
          <time dateTime={attempt.checkedAt} title={formatDate(attempt.checkedAt, locale)}>
            {formatDate(attempt.checkedAt, locale, { dateStyle: 'short', timeStyle: 'short' })}
          </time>
        </div>
      ))}
    </div>
  )
}

export function BindingRoute({
  resolved,
  accounts,
  models,
  compact = false,
}: Readonly<{
  resolved: BindingResolution
  accounts: AIAccount[]
  models: AdminModelOption[]
  compact?: boolean
}>) {
  const { t } = useTranslation()
  const binding = resolved.effectiveBinding
  const account = accounts.find((entry) => entry.id === binding?.accountId)
  const model = models.find((entry) => entry.id === binding?.modelCatalogId)
  if (!binding || !account || !model) return null
  const unavailable = !model.availableAccountIds.includes(account.id)
  const tuning = bindingRequestTuning(binding.extraParams)
  if (compact) {
    return (
      <div className="binding-summary">
        <strong>{model.modelName}</strong>
        <span>{account.label}</span>
        {unavailable && (
          <Badge tone="warning">{modelAvailabilityStateLabel('unavailable', t)}</Badge>
        )}
      </div>
    )
  }
  return (
    <>
      <div className="binding-route">
        <div>
          <span>{t('admin.aiPanel.fields.account')}</span>
          <strong>{account.label}</strong>
        </div>
        <ArrowRight size={15} />
        <div>
          <span>{t('admin.model')}</span>
          <strong>{model.modelName}</strong>
        </div>
      </div>
      {(tuning.reasoningEffort || tuning.serviceTier) && (
        <p className="binding-tuning">
          {tuning.serviceTier && (
            <Badge tone="ready">
              {t(`admin.aiPanel.requestTuning.tier.${tuning.serviceTier}`, {
                defaultValue: tuning.serviceTier,
              })}
            </Badge>
          )}
          {tuning.reasoningEffort && (
            <Badge tone="neutral">
              {t('admin.aiPanel.requestTuning.reasoningBadge', {
                effort: t(`admin.aiPanel.requestTuning.reasoning.${tuning.reasoningEffort}`, {
                  defaultValue: tuning.reasoningEffort,
                }),
              })}
            </Badge>
          )}
        </p>
      )}
      {unavailable && (
        <p className="ai-warning">
          <AlertTriangle size={13} />
          {t('admin.aiPanel.messages.bindingModelUnavailable', { model: model.modelName })}
        </p>
      )}
    </>
  )
}
