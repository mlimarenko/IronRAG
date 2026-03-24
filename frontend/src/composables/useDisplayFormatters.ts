import { useI18n } from 'vue-i18n'

export function useDisplayFormatters() {
  const { t, te, locale } = useI18n()

  function formatDateTime(value: string | null): string {
    if (!value) {
      return '—'
    }
    const parsed = new Date(value)
    if (Number.isNaN(parsed.getTime())) {
      return value
    }
    return new Intl.DateTimeFormat(locale.value || undefined, {
      dateStyle: 'medium',
      timeStyle: 'short',
    }).format(parsed)
  }

  function humanizeToken(value: string): string {
    return value
      .replaceAll('.', ' ')
      .replaceAll('_', ' ')
      .replaceAll('-', ' ')
      .trim()
      .split(/\s+/)
      .filter((part) => part.length > 0)
      .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
      .join(' ')
  }

  function shortIdentifier(value: string | null, length = 8): string {
    if (!value) {
      return '—'
    }
    return value.slice(0, length)
  }

  function enumLabel(baseKey: string, value: string | null, fallback = '—'): string {
    if (!value) {
      return fallback
    }
    const key = `${baseKey}.${value}`
    return te(key) ? t(key) : humanizeToken(value)
  }

  function graphWarningLabel(value: string | null): string | null {
    if (!value) {
      return null
    }

    switch (value) {
      case 'The canonical Arango knowledge graph generation failed.':
        return t('graph.statusDescriptions.failed')
      case 'The canonical Arango knowledge graph is still building.':
        return t('graph.statusDescriptions.building')
      case 'The canonical Arango knowledge graph is rebuilding after recent changes.':
        return t('graph.statusDescriptions.rebuilding')
      case 'The canonical Arango knowledge graph is stale.':
        return t('graph.statusDescriptions.stale')
      case 'Latest revision graph generation failed.':
        return t('graph.detailWarnings.latestRevisionGraphFailed')
      case 'Document is deleted.':
        return t('graph.detailWarnings.documentDeleted')
      default:
        if (value.startsWith('Relation contradiction state: ')) {
          return t('graph.detailWarnings.relationContradictionState', {
            value: humanizeToken(value.replace('Relation contradiction state: ', '')),
          })
        }
        if (value.startsWith('The canonical Arango knowledge generation is ')) {
          return t('graph.statusDescriptions.building')
        }
        return value
    }
  }

  function graphPropertyLabel(value: string): string {
    const mapping: Record<string, string> = {
      Type: t('graph.propertyLabels.type'),
      Support: t('graph.propertyLabels.support'),
      Aliases: t('graph.propertyLabels.aliases'),
      'Source chunks': t('graph.propertyLabels.sourceChunks'),
      Assertion: t('graph.propertyLabels.assertion'),
      'Subject entity': t('graph.propertyLabels.subjectEntity'),
      'Object entity': t('graph.propertyLabels.objectEntity'),
      'Freshness generation': t('graph.propertyLabels.freshnessGeneration'),
      State: t('graph.propertyLabels.state'),
      'Contradiction state': t('graph.propertyLabels.contradictionState'),
      'External key': t('graph.propertyLabels.externalKey'),
      'Active revision': t('graph.propertyLabels.activeRevision'),
      'Readable revision': t('graph.propertyLabels.readableRevision'),
      'Latest revision': t('graph.propertyLabels.latestRevision'),
    }

    return mapping[value] ?? value
  }

  function graphPropertyValue(key: string, value: string): string {
    if (value === '—') {
      return value
    }

    if (key === 'Type') {
      if (value === 'document' || value === 'entity' || value === 'topic') {
        return t(`graph.nodeTypes.${value}`)
      }
      return humanizeToken(value)
    }

    if (['State', 'Contradiction state'].includes(key)) {
      return humanizeToken(value)
    }

    return value
  }

  function statusBadgeLabel(status: string | null): string {
    if (!status) return '—'
    const key = `shared.statusBadge.${status}`
    return te(key) ? t(key) : humanizeToken(status)
  }

  function documentStatusLabel(status: string | null): string {
    return enumLabel('documents.statuses', status)
  }

  function mutationKindLabel(kind: string | null): string {
    return enumLabel('documents.mutationKinds', kind)
  }

  function fileFormatLabel(mime: string | null): string {
    if (!mime) return '—'
    const raw = mime.split('/').pop() ?? mime
    const normalized = raw.replace('.', '').toLowerCase()
    const key = `documents.fileFormats.${normalized}`
    return te(key) ? t(key) : raw.toUpperCase()
  }

  function documentMetadataLabel(key: string): string {
    const labelKey = `documents.details.labels.${key}`
    return te(labelKey) ? t(labelKey) : humanizeToken(key)
  }

  function inspectorMetadataLabel(key: string): string {
    const metaKey = `documents.details.${key}`
    return te(metaKey) ? t(metaKey) : humanizeToken(key)
  }

  function permissionLabel(kind: string | null): string {
    return enumLabel('admin.permissions', kind)
  }

  function bindingPurposeLabel(purpose: string | null): string {
    return enumLabel('admin.ai.bindingPurposes', purpose)
  }

  function providerStateLabel(state: string | null): string {
    return enumLabel('admin.ai.providerStates', state)
  }

  function billingUnitLabel(unit: string | null): string {
    return enumLabel('admin.ai.billingUnits', unit)
  }

  function graphHealthLabel(status: string | null): string {
    if (!status) return '—'
    const key = `graph.healthLabels.${status}`
    if (te(key)) {
      return t(key)
    }
    const fallbackKey = `graph.statuses.${status}`
    return te(fallbackKey) ? t(fallbackKey) : humanizeToken(status)
  }

  function graphNodeKindLabel(kind: string | null): string {
    if (!kind) return '—'
    const key = `graph.nodeTypes.${kind}`
    return te(key) ? t(key) : humanizeToken(kind)
  }

  function graphEvidenceLabel(count: number): string {
    return t('graph.evidenceCount', { count })
  }

  function auditActionLabel(action: string | null): string {
    return enumLabel('admin.audit.actionKinds', action)
  }

  function auditSubjectLabel(kind: string | null): string {
    return enumLabel('admin.audit.subjectKinds', kind)
  }

  function priceOriginLabel(setInWorkspace: boolean): string {
    return setInWorkspace
      ? t('admin.pricing.originWorkspace')
      : t('admin.pricing.originBaseline')
  }

  return {
    auditActionLabel,
    auditSubjectLabel,
    billingUnitLabel,
    bindingPurposeLabel,
    documentStatusLabel,
    documentMetadataLabel,
    enumLabel,
    fileFormatLabel,
    formatDateTime,
    inspectorMetadataLabel,
    graphEvidenceLabel,
    graphHealthLabel,
    graphNodeKindLabel,
    graphPropertyLabel,
    graphPropertyValue,
    graphWarningLabel,
    humanizeToken,
    mutationKindLabel,
    permissionLabel,
    priceOriginLabel,
    providerStateLabel,
    shortIdentifier,
    statusBadgeLabel,
  }
}
