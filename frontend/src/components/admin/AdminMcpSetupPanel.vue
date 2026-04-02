<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ADMIN_MCP_CLIENT_IDS } from 'src/models/ui/admin'
import type { AdminMcpClientId } from 'src/models/ui/admin'

interface McpSnippetBlock {
  id: string
  label: string
  location: string | null
  language: string
  content: string
}

interface McpGuide {
  id: AdminMcpClientId
  title: string
  vendor: string
  subtitle: string
  configLabel: string
  authLabel: string
  docsUrl: string
  steps: string[]
  note: string
  snippets: McpSnippetBlock[]
}

const props = defineProps<{
  workspaceName: string
  libraryName: string
}>()

const emit = defineEmits<{
  createToken: []
}>()

const { t, tm } = useI18n()
const selectedClientId = ref<AdminMcpClientId>('codex')
const copiedSnippetId = ref<string | null>(null)
let copiedResetTimer: number | null = null

const tokenEnvVar = 'RUSTRAG_MCP_TOKEN'
const serverName = 'rustragMemory'

const appOrigin = computed(() =>
  typeof window !== 'undefined' && window.location?.origin
    ? window.location.origin
    : 'http://127.0.0.1:19000',
)
const mcpUrl = computed(() => `${appOrigin.value}/v1/mcp`)
const capabilitiesUrl = computed(() => `${appOrigin.value}/v1/mcp/capabilities`)
const claudeDesktopSupportUrl =
  'https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp'

const sharedSnippets = computed<McpSnippetBlock[]>(() => [
  {
    id: 'shared-token-env',
    label: t('admin.mcp.common.tokenEnvSnippet'),
    location: null,
    language: 'bash',
    content: `export ${tokenEnvVar}='rtrg_...'`,
  },
  {
    id: 'shared-capabilities-probe',
    label: t('admin.mcp.common.capabilitiesProbeSnippet'),
    location: null,
    language: 'bash',
    content: [
      `curl -sS ${capabilitiesUrl.value} \\`,
      `  -H "Authorization: Bearer $${tokenEnvVar}"`,
    ].join('\n'),
  },
])

const recommendedPromptBlock = computed<McpSnippetBlock>(() => ({
  id: 'recommended-system-prompt',
  label: t('admin.mcp.recommendedPrompt.snippetLabel'),
  location: t('admin.mcp.recommendedPrompt.location'),
  language: 'text',
  content: (() => {
    const lines = tm('admin.mcp.recommendedPrompt.lines')
    return Array.isArray(lines) ? lines.map((line) => String(line)).join('\n') : ''
  })(),
}))

const guides = computed<McpGuide[]>(() => [
  {
    id: 'codex',
    title: t('admin.mcp.clients.codex.title'),
    vendor: t('admin.mcp.clients.codex.vendor'),
    subtitle: t('admin.mcp.clients.codex.subtitle'),
    configLabel: t('admin.mcp.clients.codex.configLabel'),
    authLabel: t('admin.mcp.common.authBearer'),
    docsUrl: 'https://developers.openai.com/learn/docs-mcp',
    steps: [
      t('admin.mcp.clients.codex.steps.token'),
      t('admin.mcp.clients.codex.steps.register'),
      t('admin.mcp.clients.codex.steps.verify'),
    ],
    note: t('admin.mcp.clients.codex.note'),
    snippets: [
      {
        id: 'codex-command',
        label: t('admin.mcp.common.commandSnippet'),
        location: null,
        language: 'bash',
        content: [
          `export ${tokenEnvVar}='rtrg_...'`,
          `codex mcp add ${serverName} \\`,
          `  --url ${mcpUrl.value} \\`,
          `  --bearer-token-env-var ${tokenEnvVar}`,
          '',
          'codex mcp list',
        ].join('\n'),
      },
      {
        id: 'codex-config',
        label: t('admin.mcp.common.configSnippet'),
        location: '~/.codex/config.toml',
        language: 'toml',
        content: [
          `[mcp_servers.${serverName}]`,
          `url = "${mcpUrl.value}"`,
          `bearer_token_env_var = "${tokenEnvVar}"`,
        ].join('\n'),
      },
    ],
  },
  {
    id: 'cursor',
    title: t('admin.mcp.clients.cursor.title'),
    vendor: t('admin.mcp.clients.cursor.vendor'),
    subtitle: t('admin.mcp.clients.cursor.subtitle'),
    configLabel: t('admin.mcp.clients.cursor.configLabel'),
    authLabel: t('admin.mcp.common.authBearer'),
    docsUrl: 'https://cursor.com/docs',
    steps: [
      t('admin.mcp.clients.cursor.steps.token'),
      t('admin.mcp.clients.cursor.steps.config'),
      t('admin.mcp.clients.cursor.steps.restart'),
    ],
    note: t('admin.mcp.clients.cursor.note'),
    snippets: [
      {
        id: 'cursor-config',
        label: t('admin.mcp.common.configSnippet'),
        location: '~/.cursor/mcp.json',
        language: 'json',
        content: JSON.stringify(
          {
            mcpServers: {
              [serverName]: {
                url: mcpUrl.value,
                headers: {
                  Authorization: `Bearer \${env:${tokenEnvVar}}`,
                },
              },
            },
          },
          null,
          2,
        ),
      },
    ],
  },
  {
    id: 'claude_code',
    title: t('admin.mcp.clients.claudeCode.title'),
    vendor: t('admin.mcp.clients.claudeCode.vendor'),
    subtitle: t('admin.mcp.clients.claudeCode.subtitle'),
    configLabel: t('admin.mcp.clients.claudeCode.configLabel'),
    authLabel: t('admin.mcp.common.authBearer'),
    docsUrl: 'https://code.claude.com/docs/en/mcp',
    steps: [
      t('admin.mcp.clients.claudeCode.steps.token'),
      t('admin.mcp.clients.claudeCode.steps.config'),
      t('admin.mcp.clients.claudeCode.steps.reload'),
    ],
    note: t('admin.mcp.clients.claudeCode.note'),
    snippets: [
      {
        id: 'claude-code-config',
        label: t('admin.mcp.common.configSnippet'),
        location: '.mcp.json',
        language: 'json',
        content: JSON.stringify(
          {
            mcpServers: {
              [serverName]: {
                type: 'http',
                url: mcpUrl.value,
                headers: {
                  Authorization: `Bearer \${${tokenEnvVar}}`,
                },
              },
            },
          },
          null,
          2,
        ),
      },
    ],
  },
  {
    id: 'claude_desktop',
    title: t('admin.mcp.clients.claudeDesktop.title'),
    vendor: t('admin.mcp.clients.claudeDesktop.vendor'),
    subtitle: t('admin.mcp.clients.claudeDesktop.subtitle'),
    configLabel: t('admin.mcp.clients.claudeDesktop.configLabel'),
    authLabel: t('admin.mcp.clients.claudeDesktop.authLabel'),
    docsUrl: claudeDesktopSupportUrl,
    steps: [
      t('admin.mcp.clients.claudeDesktop.steps.open'),
      t('admin.mcp.clients.claudeDesktop.steps.url'),
      t('admin.mcp.clients.claudeDesktop.steps.compatibility'),
    ],
    note: t('admin.mcp.clients.claudeDesktop.note'),
    snippets: [
      {
        id: 'claude-desktop-url',
        label: t('admin.mcp.common.connectorUrlSnippet'),
        location: t('admin.mcp.clients.claudeDesktop.configLabel'),
        language: 'text',
        content: mcpUrl.value,
      },
      {
        id: 'claude-desktop-compatibility',
        label: t('admin.mcp.common.compatibilitySnippet'),
        location: null,
        language: 'text',
        content: t('admin.mcp.clients.claudeDesktop.compatibilityBody', {
          envVar: tokenEnvVar,
        }),
      },
    ],
  },
  {
    id: 'vscode',
    title: t('admin.mcp.clients.vscode.title'),
    vendor: t('admin.mcp.clients.vscode.vendor'),
    subtitle: t('admin.mcp.clients.vscode.subtitle'),
    configLabel: t('admin.mcp.clients.vscode.configLabel'),
    authLabel: t('admin.mcp.common.authBearer'),
    docsUrl: 'https://code.visualstudio.com/docs/copilot/customization/mcp-servers',
    steps: [
      t('admin.mcp.clients.vscode.steps.token'),
      t('admin.mcp.clients.vscode.steps.config'),
      t('admin.mcp.clients.vscode.steps.enable'),
    ],
    note: t('admin.mcp.clients.vscode.note'),
    snippets: [
      {
        id: 'vscode-config',
        label: t('admin.mcp.common.configSnippet'),
        location: '.vscode/mcp.json',
        language: 'json',
        content: JSON.stringify(
          {
            servers: {
              [serverName]: {
                type: 'http',
                url: mcpUrl.value,
                headers: {
                  Authorization: `Bearer \${env:${tokenEnvVar}}`,
                },
              },
            },
          },
          null,
          2,
        ),
      },
    ],
  },
  {
    id: 'generic',
    title: t('admin.mcp.clients.generic.title'),
    vendor: t('admin.mcp.clients.generic.vendor'),
    subtitle: t('admin.mcp.clients.generic.subtitle'),
    configLabel: t('admin.mcp.clients.generic.configLabel'),
    authLabel: t('admin.mcp.common.authBearer'),
    docsUrl: 'https://modelcontextprotocol.io/docs/getting-started/intro',
    steps: [
      t('admin.mcp.clients.generic.steps.token'),
      t('admin.mcp.clients.generic.steps.endpoint'),
      t('admin.mcp.clients.generic.steps.capabilities'),
    ],
    note: t('admin.mcp.clients.generic.note'),
    snippets: [
      {
        id: 'generic-shape',
        label: t('admin.mcp.common.configShapeSnippet'),
        location: null,
        language: 'json',
        content: JSON.stringify(
          {
            type: 'http',
            url: mcpUrl.value,
            headers: {
              Authorization: 'Bearer <token>',
            },
          },
          null,
          2,
        ),
      },
    ],
  },
])

const selectedGuide = computed(
  () => guides.value.find((guide) => guide.id === selectedClientId.value) ?? guides.value[0],
)

const summary = computed(() => ({
  clients: ADMIN_MCP_CLIENT_IDS.length,
  transport: 'HTTP',
  auth: 'Bearer',
}))

const contextSummary = computed(
  () =>
    `${props.libraryName} · ${summary.value.clients} ${t('admin.mcp.summary.clients').toLowerCase()}`,
)

const primaryGuideSnippets = computed(() => selectedGuide.value?.snippets.slice(0, 1) ?? [])
const secondaryGuideSnippets = computed(() => selectedGuide.value?.snippets.slice(1) ?? [])
const quickstartSnippets = computed(() => {
  const tokenSnippet = sharedSnippets.value[0] ? [sharedSnippets.value[0]] : []
  return [...tokenSnippet, ...primaryGuideSnippets.value]
})
const quickstartSnippet = computed<McpSnippetBlock | null>(() => {
  if (quickstartSnippets.value.length === 0) {
    return null
  }
  return {
    id: `quickstart:${selectedGuide.value.id}`,
    label: t('admin.mcp.sharedQuickstartTitle'),
    location: null,
    language: 'text',
    content: quickstartSnippets.value.map((block) => block.content).join('\n\n'),
  }
})
const advancedSnippets = computed(() => {
  const capabilitySnippet = sharedSnippets.value[1] ? [sharedSnippets.value[1]] : []
  return [...capabilitySnippet, ...secondaryGuideSnippets.value]
})

async function copySnippet(block: McpSnippetBlock): Promise<void> {
  try {
    await navigator.clipboard.writeText(block.content)
    copiedSnippetId.value = block.id
    if (copiedResetTimer !== null) {
      window.clearTimeout(copiedResetTimer)
    }
    copiedResetTimer = window.setTimeout(() => {
      copiedSnippetId.value = null
      copiedResetTimer = null
    }, 1800)
  } catch {
    copiedSnippetId.value = null
  }
}

onBeforeUnmount(() => {
  if (copiedResetTimer !== null) {
    window.clearTimeout(copiedResetTimer)
  }
})
</script>

<template>
  <section class="rr-admin-workbench rr-admin-workbench--mcp">
    <div class="rr-admin-workbench__layout">
      <aside class="rr-admin-workbench__rail">
        <header class="rr-admin-workbench__pane-head">
          <div class="rr-admin-workbench__pane-copy">
            <h3>{{ $t('admin.mcp.registryTitle') }}</h3>
            <p>{{ contextSummary }}</p>
          </div>
          <button class="rr-button" type="button" @click="emit('createToken')">
            {{ $t('admin.createToken') }}
          </button>
        </header>

        <div class="rr-admin-workbench__list">
          <button
            v-for="guide in guides"
            :key="guide.id"
            type="button"
            class="rr-admin-workbench__row"
            :class="{ 'rr-admin-workbench__row--active': selectedClientId === guide.id }"
            @click="selectedClientId = guide.id"
          >
            <div class="rr-admin-workbench__row-head">
              <strong>{{ guide.title }}</strong>
              <span class="rr-status-pill is-muted">{{ guide.vendor }}</span>
            </div>
            <span class="rr-admin-workbench__row-subtitle">
              {{ guide.subtitle }}
            </span>
            <div class="rr-admin-workbench__row-meta">
              <span>{{ guide.configLabel }} · {{ guide.authLabel }}</span>
            </div>
          </button>
        </div>
      </aside>

      <section class="rr-admin-workbench__detail">
        <div v-if="selectedGuide" class="rr-admin-workbench__detail-card">
          <header class="rr-admin-workbench__detail-head">
            <div class="rr-admin-workbench__pane-copy">
              <h3>{{ selectedGuide.title }}</h3>
              <p>{{ selectedGuide.subtitle }}</p>
            </div>
            <a
              class="rr-button rr-button--ghost"
              :href="selectedGuide.docsUrl"
              target="_blank"
              rel="noreferrer"
            >
              {{ $t('admin.mcp.openDocs') }}
            </a>
          </header>

          <dl class="rr-admin-workbench__detail-grid">
            <div>
              <dt>{{ $t('admin.mcp.fields.serverName') }}</dt>
              <dd>{{ serverName }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.mcp.fields.endpoint') }}</dt>
              <dd>{{ mcpUrl }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.mcp.fields.capabilities') }}</dt>
              <dd>{{ capabilitiesUrl }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.mcp.fields.tokenEnv') }}</dt>
              <dd>{{ tokenEnvVar }}</dd>
            </div>
          </dl>

          <section class="rr-admin-workbench__detail-section">
            <article
              v-if="quickstartSnippet"
              class="rr-admin-mcp__snippet rr-admin-mcp__snippet--shared"
            >
              <header class="rr-admin-mcp__snippet-head">
                <div class="rr-admin-workbench__pane-copy">
                  <h5>{{ quickstartSnippet.label }}</h5>
                </div>
                <button
                  type="button"
                  class="rr-button rr-button--ghost rr-button--tiny"
                  @click="copySnippet(quickstartSnippet)"
                >
                  {{
                    copiedSnippetId === quickstartSnippet.id
                      ? $t('admin.mcp.copied')
                      : $t('admin.actions.copy')
                  }}
                </button>
              </header>

              <pre class="rr-admin-mcp__code"><code>{{ quickstartSnippet.content }}</code></pre>
            </article>
          </section>

          <details class="rr-admin-mcp__advanced">
            <summary>{{ $t('admin.mcp.recommendedFlowTitle') }}</summary>

            <div class="rr-admin-mcp__advanced-body">
              <section v-if="advancedSnippets.length" class="rr-admin-workbench__detail-section">
                <article
                  v-for="block in advancedSnippets"
                  :key="block.id"
                  class="rr-admin-mcp__snippet"
                >
                  <header class="rr-admin-mcp__snippet-head">
                    <div class="rr-admin-workbench__pane-copy">
                      <h5>{{ block.label }}</h5>
                      <p v-if="block.location">{{ block.location }}</p>
                    </div>
                    <button
                      type="button"
                      class="rr-button rr-button--ghost rr-button--tiny"
                      @click="copySnippet(block)"
                    >
                      {{
                        copiedSnippetId === block.id
                          ? $t('admin.mcp.copied')
                          : $t('admin.actions.copy')
                      }}
                    </button>
                  </header>

                  <pre class="rr-admin-mcp__code"><code>{{ block.content }}</code></pre>
                </article>
              </section>

              <section class="rr-admin-workbench__detail-section">
                <div class="rr-admin-workbench__pane-copy">
                  <h4>{{ $t('admin.mcp.recommendedPrompt.title') }}</h4>
                  <p>{{ $t('admin.mcp.recommendedPrompt.intro') }}</p>
                </div>

                <article class="rr-admin-mcp__snippet rr-admin-mcp__snippet--shared">
                  <header class="rr-admin-mcp__snippet-head">
                    <div class="rr-admin-workbench__pane-copy">
                      <h5>{{ recommendedPromptBlock.label }}</h5>
                      <p v-if="recommendedPromptBlock.location">
                        {{ recommendedPromptBlock.location }}
                      </p>
                    </div>
                    <button
                      type="button"
                      class="rr-button rr-button--ghost rr-button--tiny"
                      @click="copySnippet(recommendedPromptBlock)"
                    >
                      {{
                        copiedSnippetId === recommendedPromptBlock.id
                          ? $t('admin.mcp.copied')
                          : $t('admin.actions.copy')
                      }}
                    </button>
                  </header>

                  <pre
                    class="rr-admin-mcp__code"
                  ><code>{{ recommendedPromptBlock.content }}</code></pre>
                </article>
              </section>

              <section class="rr-admin-workbench__detail-section">
                <ol class="rr-admin-mcp__steps">
                  <li v-for="step in selectedGuide.steps" :key="step">
                    {{ step }}
                  </li>
                </ol>
              </section>
            </div>
          </details>

          <p class="rr-admin-workbench__feedback rr-admin-workbench__feedback--subtle">
            {{ $t('admin.mcp.originHint', { origin: appOrigin }) }}
          </p>
        </div>
      </section>
    </div>
  </section>
</template>

<style scoped lang="scss">
.rr-admin-mcp__steps {
  margin: 0;
  padding-left: 1.1rem;
  display: grid;
  gap: 0.45rem;
  color: var(--rr-text-secondary);
  font-size: 0.84rem;
  line-height: 1.55;
}

.rr-admin-mcp__snippet {
  display: grid;
  gap: 0.7rem;
  padding: 0.78rem;
  border: 1px solid rgba(226, 232, 240, 0.86);
  border-radius: 0.9rem;
  background: rgba(248, 250, 252, 0.74);
}

.rr-admin-mcp__snippet-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 0.75rem;
}

.rr-admin-mcp__snippet-head h5 {
  margin: 0;
  color: var(--rr-text-primary);
  font-size: 0.88rem;
  line-height: 1.2;
}

.rr-admin-workbench__summary {
  display: flex;
  flex-wrap: wrap;
  gap: 0.42rem;
}

.rr-admin-workbench__summary-chip {
  display: inline-flex;
  align-items: center;
  gap: 0.32rem;
  min-height: 1.9rem;
  padding: 0 0.7rem;
  border-radius: 999px;
  border: 1px solid rgba(226, 232, 240, 0.88);
  background: rgba(248, 250, 252, 0.82);
  color: var(--rr-text-secondary);
  font-size: 0.74rem;
  line-height: 1.2;
  white-space: nowrap;
}

.rr-admin-workbench__summary-chip strong {
  color: var(--rr-text-primary);
  font-size: 0.78rem;
}

.rr-admin-workbench__helper {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.78rem;
  line-height: 1.45;
}

.rr-admin-workbench--mcp .rr-admin-workbench__list {
  max-height: min(60vh, 42rem);
  overflow: auto;
  padding-right: 2px;
}

.rr-admin-mcp__advanced {
  display: grid;
  gap: 0.8rem;
  padding: 0.85rem 0.9rem;
  border: 1px solid rgba(226, 232, 240, 0.86);
  border-radius: 0.95rem;
  background: rgba(248, 250, 252, 0.58);
}

.rr-admin-mcp__advanced summary {
  cursor: pointer;
  color: var(--rr-text-primary);
  font-size: 0.84rem;
  font-weight: 700;
}

.rr-admin-mcp__advanced-body {
  display: grid;
  gap: 0.85rem;
  padding-top: 0.35rem;
}

.rr-admin-mcp__code {
  margin: 0;
  padding: 0.8rem 0.9rem;
  overflow-x: auto;
  border-radius: 0.95rem;
  border: 1px solid rgba(15, 23, 42, 0.08);
  background: #0f172a;
  color: #e2e8f0;
  font-size: 0.76rem;
  line-height: 1.55;
  font-family:
    'SFMono-Regular', 'SF Mono', 'Cascadia Code', 'JetBrains Mono', ui-monospace, monospace;
}

.rr-admin-workbench__feedback--subtle {
  padding: 0;
  border: 0;
  background: transparent;
  font-size: 0.76rem;
}

.rr-admin-mcp__code code {
  white-space: pre;
}

@media (max-width: 720px) {
  .rr-admin-workbench__context {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 6px;
  }

  .rr-admin-workbench__context-chip {
    gap: 2px;
    padding: 8px 9px;
  }

  .rr-admin-workbench__context-chip span {
    font-size: 0.6rem;
  }

  .rr-admin-workbench__context-chip strong {
    font-size: 0.78rem;
    line-height: 1.3;
  }

  .rr-admin-mcp__snippet-head {
    flex-direction: column;
    align-items: stretch;
  }

  .rr-admin-workbench__summary {
    gap: 0.36rem;
  }

  .rr-admin-workbench__summary-chip {
    min-height: 1.8rem;
    padding-inline: 0.62rem;
    font-size: 0.72rem;
  }

  .rr-admin-workbench__helper {
    font-size: 0.74rem;
    line-height: 1.4;
  }

  .rr-admin-mcp__code {
    font-size: 0.74rem;
  }
}
</style>
