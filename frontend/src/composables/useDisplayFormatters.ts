import { useI18n } from 'vue-i18n'
import { inferDocumentFormatTokenFromMime } from 'src/models/ui/documentFormats'

export function useDisplayFormatters() {
  const i18n = useI18n()

  function formatDateTime(value: string | null): string {
    if (!value) {
      return '—'
    }
    const parsed = new Date(value)
    if (Number.isNaN(parsed.getTime())) {
      return value
    }
    return new Intl.DateTimeFormat(i18n.locale.value || undefined, {
      dateStyle: 'medium',
      timeStyle: 'short',
    }).format(parsed)
  }

  function formatCompactDateTime(value: string | null): string {
    if (!value) {
      return '—'
    }
    const parsed = new Date(value)
    if (Number.isNaN(parsed.getTime())) {
      return value
    }

    const now = new Date()
    const includeYear = parsed.getFullYear() !== now.getFullYear()

    return new Intl.DateTimeFormat(i18n.locale.value || undefined, {
      day: 'numeric',
      month: 'short',
      ...(includeYear ? { year: 'numeric' } : {}),
      hour: '2-digit',
      minute: '2-digit',
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
    return i18n.te(key) ? i18n.t(key) : humanizeToken(value)
  }

  function graphWarningLabel(value: string | null): string | null {
    if (!value) {
      return null
    }

    const normalized = value.trim().toLowerCase()
    if (!normalized || normalized === 'unknown' || normalized === 'none' || normalized === 'n/a') {
      return null
    }

    switch (value) {
      case 'The canonical Arango knowledge graph generation failed.':
        return i18n.t('graph.statusDescriptions.failed')
      case 'The canonical Arango knowledge graph is still building.':
        return i18n.t('graph.statusDescriptions.building')
      case 'The canonical Arango knowledge graph is rebuilding after recent changes.':
        return i18n.t('graph.statusDescriptions.rebuilding')
      case 'The canonical Arango knowledge graph is stale.':
        return i18n.t('graph.statusDescriptions.stale')
      case 'Latest revision graph generation failed.':
        return i18n.t('graph.detailWarnings.latestRevisionGraphFailed')
      case 'Document is deleted.':
        return i18n.t('graph.detailWarnings.documentDeleted')
      default:
        if (value.startsWith('Relation contradiction state: ')) {
          const contradictionState = value.replace('Relation contradiction state: ', '').trim()
          const normalizedContradictionState = contradictionState.toLowerCase()
          if (
            !normalizedContradictionState ||
            normalizedContradictionState === 'unknown' ||
            normalizedContradictionState === 'none' ||
            normalizedContradictionState === 'n/a'
          ) {
            return null
          }
          return i18n.t('graph.detailWarnings.relationContradictionState', {
            value: humanizeToken(contradictionState),
          })
        }
        if (value.startsWith('The canonical Arango knowledge generation is ')) {
          return i18n.t('graph.statusDescriptions.building')
        }
        return value
    }
  }

  function graphPropertyLabel(value: string): string {
    const mapping: Record<string, string> = {
      Type: i18n.t('graph.propertyLabels.type'),
      Support: i18n.t('graph.propertyLabels.support'),
      Aliases: i18n.t('graph.propertyLabels.aliases'),
      'Source chunks': i18n.t('graph.propertyLabels.sourceChunks'),
      Assertion: i18n.t('graph.propertyLabels.assertion'),
      'Subject entity': i18n.t('graph.propertyLabels.subjectEntity'),
      'Object entity': i18n.t('graph.propertyLabels.objectEntity'),
      'Freshness generation': i18n.t('graph.propertyLabels.freshnessGeneration'),
      State: i18n.t('graph.propertyLabels.state'),
      'Contradiction state': i18n.t('graph.propertyLabels.contradictionState'),
      'External key': i18n.t('graph.propertyLabels.externalKey'),
      'Active revision': i18n.t('graph.propertyLabels.activeRevision'),
      'Readable revision': i18n.t('graph.propertyLabels.readableRevision'),
      'Latest revision': i18n.t('graph.propertyLabels.latestRevision'),
    }

    return mapping[value] ?? value
  }

  function graphPropertyValue(key: string, value: string): string {
    if (value === '—') {
      return value
    }

    if (key === 'Type') {
      if (value === 'document' || value === 'entity' || value === 'topic') {
        return i18n.t(`graph.nodeTypes.${value}`)
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
    return i18n.te(key) ? i18n.t(key) : humanizeToken(status)
  }

  function documentStatusLabel(status: string | null): string {
    if (status === 'ready') {
      return enumLabel('documents.readinessKinds', 'graph_ready')
    }
    if (status === 'ready_no_graph') {
      return enumLabel('documents.readinessKinds', 'graph_sparse')
    }
    return enumLabel('documents.statuses', status)
  }

  function documentReadinessLabel(readiness: string | null): string {
    return enumLabel('documents.readinessKinds', readiness)
  }

  function documentGraphCoverageLabel(coverage: string | null): string {
    return enumLabel('documents.graphCoverageKinds', coverage)
  }

  function mutationKindLabel(kind: string | null): string {
    return enumLabel('documents.mutationKinds', kind)
  }

  function fileFormatLabel(mime: string | null): string {
    if (!mime) return '—'
    const normalized = inferDocumentFormatTokenFromMime(mime)
    const raw = mime.split('/').pop() ?? mime
    if (!normalized) return raw.toUpperCase()
    const key = `documents.fileFormats.${normalized}`
    return i18n.te(key) ? i18n.t(key) : raw.toUpperCase()
  }

  function documentMetadataLabel(key: string): string {
    const labelKey = `documents.details.labels.${key}`
    return i18n.te(labelKey) ? i18n.t(labelKey) : humanizeToken(key)
  }

  function uploadFailureLabel(value: string | null): string | null {
    if (!value) {
      return null
    }
    const normalized = value.trim()
    if (!normalized) {
      return null
    }
    const key = `documents.uploadReport.rejectionKinds.${normalized}`
    if (i18n.te(key)) {
      return i18n.t(key)
    }
    return /\s/.test(normalized) ? normalized : humanizeToken(normalized)
  }

  function inspectorMetadataLabel(key: string): string {
    const metaKey = `documents.details.${key}`
    return i18n.te(metaKey) ? i18n.t(metaKey) : humanizeToken(key)
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
    if (i18n.te(key)) {
      return i18n.t(key)
    }
    const fallbackKey = `graph.statuses.${status}`
    return i18n.te(fallbackKey) ? i18n.t(fallbackKey) : humanizeToken(status)
  }

  function graphNodeKindLabel(kind: string | null): string {
    if (!kind) return '—'
    const key = `graph.nodeTypes.${kind}`
    return i18n.te(key) ? i18n.t(key) : humanizeToken(kind)
  }

  function graphEvidenceLabel(count: number): string {
    return i18n.t('graph.evidenceCount', { count })
  }

  function auditActionLabel(action: string | null): string {
    return enumLabel('admin.audit.actionKinds', action)
  }

  function auditSubjectLabel(kind: string | null): string {
    return enumLabel('admin.audit.subjectKinds', kind)
  }

  function priceOriginLabel(setInWorkspace: boolean): string {
    return setInWorkspace
      ? i18n.t('admin.pricing.originWorkspace')
      : i18n.t('admin.pricing.originBaseline')
  }

  return {
    auditActionLabel,
    auditSubjectLabel,
    billingUnitLabel,
    bindingPurposeLabel,
    documentStatusLabel,
    documentMetadataLabel,
    documentGraphCoverageLabel,
    documentReadinessLabel,
    enumLabel,
    fileFormatLabel,
    formatCompactDateTime,
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
    uploadFailureLabel,
    shortIdentifier,
    statusBadgeLabel,
  }
}
