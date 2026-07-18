import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Loader2, Pencil, Plus, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import { z } from 'zod'

import { adminApi } from '@/shared/api'
import { Button } from '@/shared/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/shared/components/ui/dialog'
import { SelectItem } from '@/shared/components/ui/select'
import { errorMessage } from '@/shared/lib/errorMessage'
import type { AIModelOption, PricingRule } from '@/shared/types'
import { FormInputField, FormSelectField, nonEmptyString, useTypedForm } from '@/shared/forms'

type PriceOverrideDialogProps = Readonly<{
  open: boolean
  onOpenChange: (open: boolean) => void
  model: AIModelOption | null
  prices: PricingRule[]
  activeWorkspaceId: string | undefined
  invalidatePrices: () => Promise<void>
}>

export function PriceOverrideDialog({
  open,
  onOpenChange,
  model,
  prices,
  activeWorkspaceId,
  invalidatePrices,
}: PriceOverrideDialogProps) {
  const { t } = useTranslation()
  const [editorOpen, setEditorOpen] = useState(false)
  const [editingRule, setEditingRule] = useState<PricingRule | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<PricingRule | null>(null)
  const [deleting, setDeleting] = useState(false)

  const rows = useMemo(
    () => (model ? prices.filter((entry) => entry.modelCatalogId === model.id) : []),
    [model, prices],
  )

  const pricingSchema = useMemo(
    () =>
      z.object({
        billingUnit: nonEmptyString(t('admin.selectBillingUnit')),
        currency: nonEmptyString(t('admin.currency')),
        effectiveFrom: nonEmptyString(t('admin.effectiveFrom')),
        effectiveTo: z.string(),
        unitPrice: nonEmptyString(t('admin.unitPrice')),
      }),
    [t],
  )
  const pricingForm = useTypedForm({
    schema: pricingSchema,
    defaultValues: {
      billingUnit: '',
      currency: 'USD',
      effectiveFrom: '',
      effectiveTo: '',
      unitPrice: '',
    },
    mode: 'onChange',
  })

  const resetPricingEditor = () => {
    setEditorOpen(false)
    setEditingRule(null)
    pricingForm.reset({
      billingUnit: '',
      currency: 'USD',
      effectiveFrom: '',
      effectiveTo: '',
      unitPrice: '',
    })
  }

  const openNewPricingEditor = () => {
    setEditingRule(null)
    pricingForm.reset({
      billingUnit: '',
      currency: 'USD',
      effectiveFrom: '',
      effectiveTo: '',
      unitPrice: '',
    })
    setEditorOpen(true)
  }

  const openPricingEditor = (rule: PricingRule) => {
    setEditingRule(rule)
    pricingForm.reset({
      billingUnit: rule.billingUnit,
      currency: rule.currency,
      effectiveFrom: rule.effectiveFrom,
      effectiveTo: rule.effectiveTo ?? '',
      unitPrice: String(rule.unitPrice),
    })
    setEditorOpen(true)
  }

  const handleSave = pricingForm.submitWithMutation(
    {
      mutateAsync: async (values) => {
        if (!model || (!editingRule && !activeWorkspaceId)) {
          return
        }
        const body = {
          modelCatalogId: model.id,
          billingUnit: values.billingUnit,
          unitPrice: values.unitPrice,
          currencyCode: values.currency,
          effectiveFrom: new Date(values.effectiveFrom).toISOString(),
          effectiveTo: values.effectiveTo ? new Date(values.effectiveTo).toISOString() : null,
        }
        if (editingRule) {
          await adminApi.updatePriceOverride(editingRule.id, body)
        } else {
          await adminApi.createPriceOverride({
            workspaceId: activeWorkspaceId as string,
            ...body,
          })
        }
        toast.success(
          editingRule ? t('admin.pricingOverrideUpdated') : t('admin.pricingOverrideCreated'),
        )
        resetPricingEditor()
        await invalidatePrices()
      },
    },
    {
      errorMessage: (err) => errorMessage(err, t('admin.savePricingFailed')),
    },
  )

  const deletePriceOverride = async () => {
    if (!deleteTarget || deleting) return
    setDeleting(true)
    try {
      await adminApi.deletePriceOverride(deleteTarget.id)
      setDeleteTarget(null)
      toast.success(t('admin.pricingOverrideDeleted'))
      await invalidatePrices()
    } catch (err) {
      toast.error(errorMessage(err, t('admin.deletePricingFailed')))
    } finally {
      setDeleting(false)
    }
  }

  return (
    <>
      <Dialog
        open={open}
        onOpenChange={(next) => {
          if (!next) {
            resetPricingEditor()
          }
          onOpenChange(next)
        }}
      >
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>
              {t('admin.pricingOverrideDialogTitle', { model: model?.modelName ?? '' })}
            </DialogTitle>
            <DialogDescription>{t('admin.pricingOverrideDescription')}</DialogDescription>
          </DialogHeader>

          {!editorOpen ? (
            <div className="space-y-3">
              {rows.length === 0 ? (
                <p className="text-sm text-muted-foreground">{t('admin.noPricingData')}</p>
              ) : (
                <div className="space-y-2">
                  {rows.map((rule) => {
                    const editable =
                      rule.sourceOrigin === 'workspace_override' &&
                      rule.workspaceId === activeWorkspaceId
                    return (
                      <div
                        key={rule.id}
                        className="flex items-center justify-between gap-3 rounded-lg border border-border/70 px-3 py-2"
                      >
                        <div className="min-w-0">
                          <div className="text-sm font-semibold tabular-nums">
                            ${rule.unitPrice.toFixed(2)} {rule.currency}
                          </div>
                          <div className="truncate text-2xs text-muted-foreground">
                            {rule.billingUnit.replace(/_/g, ' ')} · {rule.sourceOrigin}
                          </div>
                        </div>
                        {editable && (
                          <div className="flex shrink-0 items-center gap-1">
                            <Button
                              type="button"
                              size="icon"
                              variant="ghost"
                              className="h-8 w-8"
                              aria-label={`${t('admin.edit')}: ${rule.billingUnit}`}
                              onClick={() => openPricingEditor(rule)}
                            >
                              <Pencil className="h-3.5 w-3.5" />
                            </Button>
                            <Button
                              type="button"
                              size="icon"
                              variant="ghost"
                              className="h-8 w-8 text-status-failed hover:text-status-failed"
                              aria-label={`${t('admin.delete')}: ${rule.billingUnit}`}
                              onClick={() => setDeleteTarget(rule)}
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </Button>
                          </div>
                        )}
                      </div>
                    )
                  })}
                </div>
              )}
              <Button size="sm" onClick={openNewPricingEditor} disabled={!activeWorkspaceId}>
                <Plus className="mr-1.5 h-3.5 w-3.5" /> {t('admin.override')}
              </Button>
            </div>
          ) : (
            <div className="space-y-4">
              <h4 className="text-sm font-bold tracking-tight">
                {editingRule ? t('admin.editPricingOverride') : t('admin.addPricingOverride')}
              </h4>
              <FormSelectField
                control={pricingForm.control}
                formState={pricingForm.formState}
                id="admin-pricing-billing-unit"
                label={t('admin.billingUnit')}
                name="billingUnit"
                placeholder={t('admin.selectBillingUnit')}
              >
                <SelectItem value="per_1m_input_tokens">{t('admin.per1mInputTokens')}</SelectItem>
                <SelectItem value="per_1m_cached_input_tokens">
                  {t('admin.per1mCachedInputTokens')}
                </SelectItem>
                <SelectItem value="per_1m_output_tokens">{t('admin.per1mOutputTokens')}</SelectItem>
              </FormSelectField>
              <div className="grid grid-cols-2 gap-3">
                <FormInputField
                  formState={pricingForm.formState}
                  id="admin-pricing-unit-price"
                  label={t('admin.unitPrice')}
                  name="unitPrice"
                  registration={pricingForm.register('unitPrice')}
                  type="number"
                  step="0.01"
                  placeholder={t('admin.aiPanel.placeholders.priceAmount')}
                />
                <FormInputField
                  formState={pricingForm.formState}
                  id="admin-pricing-currency"
                  label={t('admin.currency')}
                  name="currency"
                  registration={pricingForm.register('currency')}
                />
              </div>
              <div className="grid grid-cols-2 gap-3">
                <FormInputField
                  formState={pricingForm.formState}
                  id="admin-pricing-effective-from"
                  label={t('admin.effectiveFrom')}
                  name="effectiveFrom"
                  registration={pricingForm.register('effectiveFrom')}
                  type="date"
                />
                <FormInputField
                  formState={pricingForm.formState}
                  id="admin-pricing-effective-to"
                  label={t('admin.effectiveTo')}
                  name="effectiveTo"
                  registration={pricingForm.register('effectiveTo')}
                  type="date"
                />
              </div>
            </div>
          )}

          <DialogFooter>
            {editorOpen ? (
              <>
                <Button variant="outline" onClick={() => setEditorOpen(false)}>
                  {t('admin.cancel')}
                </Button>
                <Button
                  disabled={!pricingForm.formState.isValid || pricingForm.formState.isSubmitting}
                  onClick={async () => {
                    await handleSave()
                  }}
                >
                  {pricingForm.formState.isSubmitting ? (
                    <>
                      <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}
                    </>
                  ) : (
                    t('admin.save')
                  )}
                </Button>
              </>
            ) : (
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                {t('common.close')}
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog
        open={Boolean(deleteTarget)}
        onOpenChange={(next) => {
          if (!next) setDeleteTarget(null)
        }}
      >
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>{t('admin.deletePricingOverrideTitle')}</DialogTitle>
            <DialogDescription>
              {t('admin.deletePricingOverrideDescription', { model: model?.modelName ?? '' })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>
              {t('admin.cancel')}
            </Button>
            <Button
              variant="destructive"
              disabled={deleting}
              onClick={async () => {
                await deletePriceOverride()
              }}
            >
              {deleting ? (
                <>
                  <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}
                </>
              ) : (
                t('admin.delete')
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}
