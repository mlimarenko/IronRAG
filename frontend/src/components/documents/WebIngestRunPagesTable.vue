<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import StatusBadge from 'src/components/design-system/StatusBadge.vue'
import type { WebCandidateState, WebDiscoveredPage } from 'src/models/ui/documents'

const props = defineProps<{
  pages: WebDiscoveredPage[]
}>()

const emit = defineEmits<{
  openDocument: [documentId: string]
}>()

const { t, te } = useI18n()

const pagesById = computed(() => {
  const map = new Map<string, WebDiscoveredPage>()
  for (const page of props.pages) {
    map.set(page.candidateId, page)
  }
  return map
})

function pageDisplayLabel(page: WebDiscoveredPage): string {
  const value = page.finalUrl ?? page.canonicalUrl ?? page.normalizedUrl
  try {
    const parsed = new URL(value)
    const path = parsed.pathname === '/' ? '' : parsed.pathname
    return `${parsed.host}${path}`
  } catch {
    return value
  }
}

function stateLabel(state: WebCandidateState): string {
  const key = `documents.webRuns.candidateStates.${state}`
  return te(key) ? t(key) : state
}

function stateTone(
  state: WebCandidateState,
): 'queued' | 'processing' | 'ready' | 'warning' | 'failed' | 'partial' | 'disabled' | 'info' {
  switch (state) {
    case 'processed':
      return 'ready'
    case 'queued':
      return 'queued'
    case 'processing':
      return 'processing'
    case 'duplicate':
      return 'info'
    case 'excluded':
    case 'blocked':
    case 'canceled':
      return 'disabled'
    case 'failed':
      return 'failed'
    case 'eligible':
    case 'discovered':
    default:
      return 'warning'
  }
}

function hostLabel(value: 'same_host' | 'external'): string {
  return t(`documents.webRuns.hosts.${value}`)
}

function reasonLabel(reason: string | null): string {
  if (!reason) {
    return '—'
  }
  const key = `documents.webRuns.reasons.${reason}`
  return te(key) ? t(key) : reason
}

function referrerLabel(page: WebDiscoveredPage): string {
  if (!page.referrerCandidateId) {
    return '—'
  }
  const referrer = pagesById.value.get(page.referrerCandidateId)
  return referrer ? pageDisplayLabel(referrer) : page.referrerCandidateId
}
</script>

<template>
  <section class="rr-web-run-pages">
    <div class="rr-web-run-pages__head">
      <strong>{{ $t('documents.webRuns.pages.title') }}</strong>
      <span>{{ pages.length }}</span>
    </div>

    <div v-if="pages.length === 0" class="rr-web-run-pages__empty">
      {{ $t('documents.webRuns.pages.empty') }}
    </div>

    <template v-else>
      <div class="rr-web-run-pages__cards">
        <article
          v-for="page in pages"
          :key="page.candidateId"
          class="rr-web-run-pages__card"
          :data-state="page.candidateState"
          :data-host="page.hostClassification"
        >
          <div class="rr-web-run-pages__card-head">
            <div class="rr-web-run-pages__page">
              <strong>{{ pageDisplayLabel(page) }}</strong>
              <span>{{ page.finalUrl ?? page.canonicalUrl ?? page.normalizedUrl }}</span>
            </div>
            <StatusBadge
              :kind="stateTone(page.candidateState)"
              :label="stateLabel(page.candidateState)"
            />
          </div>

          <dl class="rr-web-run-pages__card-meta">
            <div>
              <dt>{{ $t('documents.webRuns.pages.columns.depth') }}</dt>
              <dd>{{ page.depth }}</dd>
            </div>
            <div>
              <dt>{{ $t('documents.webRuns.pages.columns.host') }}</dt>
              <dd>{{ hostLabel(page.hostClassification) }}</dd>
            </div>
            <div>
              <dt>{{ $t('documents.webRuns.pages.columns.referrer') }}</dt>
              <dd>{{ referrerLabel(page) }}</dd>
            </div>
            <div>
              <dt>{{ $t('documents.webRuns.pages.columns.reason') }}</dt>
              <dd>{{ reasonLabel(page.classificationReason) }}</dd>
            </div>
          </dl>

          <button
            v-if="page.documentId"
            type="button"
            class="rr-button rr-button--ghost rr-button--tiny"
            @click="emit('openDocument', page.documentId)"
          >
            {{ $t('documents.webRuns.pages.openDocument') }}
          </button>
        </article>
      </div>

      <div class="rr-web-run-pages__table-wrap">
        <table class="rr-web-run-pages__table">
          <thead>
            <tr>
              <th>{{ $t('documents.webRuns.pages.columns.page') }}</th>
              <th>{{ $t('documents.webRuns.pages.columns.state') }}</th>
              <th>{{ $t('documents.webRuns.pages.columns.depth') }}</th>
              <th>{{ $t('documents.webRuns.pages.columns.host') }}</th>
              <th>{{ $t('documents.webRuns.pages.columns.referrer') }}</th>
              <th>{{ $t('documents.webRuns.pages.columns.reason') }}</th>
              <th>{{ $t('documents.webRuns.pages.columns.result') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="page in pages"
              :key="page.candidateId"
              class="rr-web-page-row"
              :data-state="page.candidateState"
              :data-host="page.hostClassification"
            >
              <td class="rr-web-run-pages__page">
                <strong>{{ pageDisplayLabel(page) }}</strong>
                <span>{{ page.finalUrl ?? page.canonicalUrl ?? page.normalizedUrl }}</span>
              </td>
              <td>
                <StatusBadge
                  :kind="stateTone(page.candidateState)"
                  :label="stateLabel(page.candidateState)"
                />
              </td>
              <td>{{ page.depth }}</td>
              <td>{{ hostLabel(page.hostClassification) }}</td>
              <td>{{ referrerLabel(page) }}</td>
              <td>{{ reasonLabel(page.classificationReason) }}</td>
              <td>
                <button
                  v-if="page.documentId"
                  type="button"
                  class="rr-button rr-button--ghost rr-button--tiny"
                  @click="emit('openDocument', page.documentId)"
                >
                  {{ $t('documents.webRuns.pages.openDocument') }}
                </button>
                <span v-else>—</span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </template>
  </section>
</template>

<style scoped lang="scss">
.rr-web-run-pages {
  display: grid;
  gap: 10px;
}

.rr-web-run-pages__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  color: var(--rr-text-secondary);
  font-size: 0.8rem;
}

.rr-web-run-pages__empty {
  padding: 12px;
  border: 1px dashed rgba(148, 163, 184, 0.5);
  border-radius: 12px;
  color: var(--rr-text-secondary);
  font-size: 0.86rem;
}

.rr-web-run-pages__cards {
  display: none;
}

.rr-web-run-pages__table-wrap {
  overflow: auto;
  border: 1px solid rgba(226, 232, 240, 0.9);
  border-radius: 14px;
  background: rgba(255, 255, 255, 0.92);
}

.rr-web-run-pages__table {
  width: 100%;
  min-width: 840px;
  border-collapse: collapse;
}

.rr-web-run-pages__table th,
.rr-web-run-pages__table td {
  padding: 10px 11px;
  border-bottom: 1px solid rgba(226, 232, 240, 0.7);
  text-align: left;
  vertical-align: top;
  font-size: 0.78rem;
}

.rr-web-run-pages__table th {
  color: var(--rr-text-secondary);
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.03em;
  text-transform: uppercase;
}

.rr-web-run-pages__table tbody tr:last-child td {
  border-bottom: none;
}

.rr-web-run-pages__page {
  display: grid;
  gap: 3px;
  min-width: 240px;
}

.rr-web-run-pages__page strong {
  color: var(--rr-text-primary);
  font-size: 0.82rem;
}

.rr-web-run-pages__page span {
  color: var(--rr-text-secondary);
  word-break: break-all;
}

@media (max-width: 760px) {
  .rr-web-run-pages__cards {
    display: grid;
    gap: 8px;
  }

  .rr-web-run-pages__card {
    display: grid;
    gap: 8px;
    padding: 10px;
    border: 1px solid rgba(226, 232, 240, 0.86);
    border-radius: 12px;
    background: rgba(255, 255, 255, 0.92);
  }

  .rr-web-run-pages__card-head {
    display: grid;
    gap: 8px;
  }

  .rr-web-run-pages__card-meta {
    display: grid;
    gap: 6px;
    margin: 0;
  }

  .rr-web-run-pages__card-meta div {
    display: grid;
    gap: 2px;
  }

  .rr-web-run-pages__card-meta dt {
    color: var(--rr-text-secondary);
    font-size: 0.7rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .rr-web-run-pages__card-meta dd {
    margin: 0;
    color: var(--rr-text-primary);
    font-size: 0.76rem;
    line-height: 1.36;
    word-break: break-word;
  }

  .rr-web-run-pages__table-wrap {
    display: none;
  }
}
</style>
