import { useTranslation } from 'react-i18next'

import { Button } from '@/shared/ui'
import type { ManagedReasoningMode, ReasoningJournal } from './bindingForm'

/**
 * The typed reasoning selection field of the binding dialog.
 *
 * It renders what `deriveReasoningJournal` decided: a select for a confirmed
 * exact pair, or a read-only explanation of why nothing is selectable. Every
 * blocked state names its reason and offers the action that resolves it, so a
 * disabled Save is never a dead end.
 */
export function ReasoningField({
  formId,
  journal,
  mode,
  onSelectMode,
  onConfirm,
  onUnsetStored,
  onUnsetPending,
  onRemoveConflict,
  onRemoveManagedKey,
}: Readonly<{
  formId: string
  journal: ReasoningJournal
  mode: ManagedReasoningMode
  onSelectMode: (mode: ManagedReasoningMode) => void
  onConfirm: () => void
  onUnsetStored: () => void
  onUnsetPending: () => void
  onRemoveConflict: (parameter: string) => void
  onRemoveManagedKey: () => void
}>) {
  const { t } = useTranslation()
  const labelId = `${formId}-reasoning-label`
  const descriptionId = `${formId}-reasoning-description`
  const hintId = `${formId}-reasoning-hint`
  let conflictText = ''
  let conflictParameter = ''
  if (journal.rawConflict) {
    conflictParameter = journal.rawConflict.parameter
    conflictText =
      journal.rawConflict.kind === 'other_control'
        ? t('admin.aiPanel.reasoning.rawConflictOther', { parameter: conflictParameter })
        : t('admin.aiPanel.reasoning.rawConflictValue', {
            parameter: conflictParameter,
            wire: journal.wire ? t(`admin.aiPanel.reasoning.wire.${journal.wire}`) : '',
          })
  }
  return (
    <div className="field field--wide">
      <span id={labelId}>{t('admin.aiPanel.reasoning.label')}</span>
      {journal.confirmed ? (
        <select
          className="input"
          aria-labelledby={labelId}
          aria-describedby={hintId}
          disabled={journal.storedInvalid}
          value={mode}
          onChange={(event) => onSelectMode(event.target.value as ManagedReasoningMode)}
        >
          <option value="">{t('admin.aiPanel.reasoning.unset')}</option>
          {journal.modes.map((candidate) => (
            <option key={candidate} value={candidate}>
              {t(`admin.aiPanel.reasoning.mode.${candidate}`)}
            </option>
          ))}
        </select>
      ) : (
        <p className="ai-empty" id={descriptionId}>
          {t(`admin.aiPanel.reasoning.readonly.${journal.readonlyKey}`)}
        </p>
      )}
      <small className="field-hint" id={hintId}>
        {t('admin.aiPanel.reasoning.hint')}
      </small>
      {journal.storedInvalid && (
        <p className="ai-inline-error" role="alert">
          {t('admin.aiPanel.reasoning.storedInvalid')}{' '}
          <Button size="sm" type="button" onClick={onUnsetStored}>
            {t('admin.aiPanel.reasoning.unsetAction')}
          </Button>
        </p>
      )}
      {journal.modeUnsupported && (
        <p className="ai-inline-error" role="alert">
          {t('admin.aiPanel.reasoning.unsupported')}{' '}
          <Button size="sm" type="button" onClick={onUnsetPending}>
            {t('admin.aiPanel.reasoning.unsetAction')}
          </Button>
        </p>
      )}
      {journal.targetChanged && journal.confirmed && (
        <p className="ai-inline-error" role="alert">
          {t('admin.aiPanel.reasoning.targetChanged')}{' '}
          <Button size="sm" type="button" onClick={onConfirm}>
            {t('admin.aiPanel.reasoning.confirmAction')}
          </Button>{' '}
          <Button size="sm" type="button" onClick={onUnsetPending}>
            {t('admin.aiPanel.reasoning.unsetAction')}
          </Button>
        </p>
      )}
      {journal.rawConflict && (
        <p className="ai-inline-error" role="alert">
          {conflictText}{' '}
          <Button size="sm" type="button" onClick={() => onRemoveConflict(conflictParameter)}>
            {t('admin.aiPanel.reasoning.removeParameter', { parameter: conflictParameter })}
          </Button>
        </p>
      )}
      {journal.managedKeyTyped && (
        <p className="ai-inline-error" role="alert">
          {t('admin.aiPanel.reasoning.managedKeyInRaw')}{' '}
          <Button size="sm" type="button" onClick={onRemoveManagedKey}>
            {t('admin.aiPanel.reasoning.removeManagedKey')}
          </Button>
        </p>
      )}
    </div>
  )
}
