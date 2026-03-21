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

  return {
    enumLabel,
    formatDateTime,
    graphPropertyLabel,
    graphPropertyValue,
    graphWarningLabel,
    humanizeToken,
    shortIdentifier,
  }
}
