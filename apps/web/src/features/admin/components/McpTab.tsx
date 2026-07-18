import { useQuery } from '@tanstack/react-query'
import type { TFunction } from 'i18next'
import { Link } from 'react-router-dom'
import { ArrowRight, Copy, Terminal, Code2, Brain, KeyRound } from 'lucide-react'
import { Button } from '@/shared/components/ui/button'
import { DataState } from '@/shared/components/DataState'
import { queries } from '@/shared/api'

type McpClientConfig = {
  name: string
  icon: typeof Terminal
  config: string
}

// All snippets assume the MCP Streamable HTTP transport (spec 2025-11-25)
// that IronRAG now speaks natively. No stdio proxy, no bespoke SSE
// endpoint — just the canonical `POST/GET/DELETE /v1/mcp` URL plus a
// bearer token. `${IRONRAG_MCP_TOKEN}` placeholder reminds operators to
// store the token in an env var, not inline in their dotfile.
function getMcpConfigs(origin: string): McpClientConfig[] {
  const mcpUrl = `${origin}/v1/mcp`
  return [
    {
      name: 'Claude Code',
      icon: Terminal,
      config: `claude mcp add ironrag ${mcpUrl} \\\n  --transport http \\\n  --header "Authorization: Bearer $IRONRAG_MCP_TOKEN"`,
    },
    {
      name: 'Claude Desktop',
      icon: Brain,
      config: `{\n  "mcpServers": {\n    "ironrag": {\n      "url": "${mcpUrl}",\n      "headers": {\n        "Authorization": "Bearer \${IRONRAG_MCP_TOKEN}"\n      }\n    }\n  }\n}`,
    },
    {
      name: 'Cursor',
      icon: Code2,
      config: `// .cursor/mcp.json\n{\n  "mcpServers": {\n    "ironrag": {\n      "url": "${mcpUrl}",\n      "headers": {\n        "Authorization": "Bearer \${env:IRONRAG_MCP_TOKEN}"\n      }\n    }\n  }\n}`,
    },
    {
      name: 'Codex',
      icon: Terminal,
      config: `# ~/.codex/config.toml\n[mcp_servers.ironrag]\nurl = "${mcpUrl}"\nbearer_token_env_var = "IRONRAG_MCP_TOKEN"`,
    },
    {
      name: 'VS Code',
      icon: Code2,
      config: `// .vscode/mcp.json\n{\n  "servers": {\n    "ironrag": {\n      "type": "http",\n      "url": "${mcpUrl}",\n      "headers": {\n        "Authorization": "Bearer \${env:IRONRAG_MCP_TOKEN}"\n      }\n    }\n  }\n}`,
    },
    {
      name: 'OpenClaw',
      icon: Terminal,
      config: `openclaw mcp set ironrag '{"url":"${mcpUrl}","headers":{"Authorization":"Bearer $IRONRAG_MCP_TOKEN"}}'`,
    },
    {
      name: 'Hermes',
      icon: Brain,
      config: `// ~/.hermes/mcp.json
{
  "mcpServers": {
    "ironrag": {
      "url": "${mcpUrl}",
      "headers": {
        "Authorization": "Bearer \${IRONRAG_MCP_TOKEN}"
      }
    }
  }
}`,
    },
  ]
}

type McpConnectGuideProps = Readonly<{
  t: TFunction
  libraryId?: string
}>

/**
 * Instance-level MCP connect guide (RM-06): server URLs, token/scope note,
 * parity notice, the recommended system-prompt preview, and per-client config
 * snippets. This is not per-library — a client connects with a bearer token
 * whose scope (system / workspace / library set) governs what it can reach, so
 * the guide lives on the System page. The per-library document-hint toggle is a
 * separate library setting and lives in the Libraries catalog inspector.
 */
export function McpConnectGuide({ t, libraryId }: McpConnectGuideProps) {
  const promptQuery = useQuery({
    ...queries.getAssistantSystemPromptOptions(libraryId ? { query: { libraryId } } : {}),
  })
  const promptResponse = promptQuery.data as
    { rendered?: string | null; template?: string } | undefined
  const systemPrompt = promptResponse?.rendered ?? promptResponse?.template ?? null

  const origin = window.location.origin
  const configs = getMcpConfigs(origin)

  return (
    <>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 text-xs">
        <div className="workbench-surface p-4">
          <div className="section-label mb-1.5">{t('admin.mcpServerUrl')}</div>
          <code className="font-mono text-xs font-bold">{origin}/v1/mcp</code>
        </div>
        <div className="workbench-surface p-4">
          <div className="section-label mb-1.5">{t('admin.capabilitiesProbe')}</div>
          <code className="font-mono text-xs font-bold">{origin}/v1/mcp/capabilities</code>
        </div>
      </div>
      <div className="workbench-surface flex items-start gap-3 p-4 text-xs leading-relaxed">
        <div className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-xl bg-surface-sunken">
          <KeyRound className="h-4 w-4 text-muted-foreground" />
        </div>
        <div className="min-w-0">
          <div className="section-label mb-1.5">{t('admin.mcpScopeTitle')}</div>
          <p className="text-muted-foreground">{t('admin.mcpScopeNote')}</p>
          <Link
            to="/admin/access"
            className="mt-2 inline-flex items-center gap-1 text-xs font-semibold text-primary hover:underline"
          >
            {t('admin.nav.access')}
            <ArrowRight className="h-3.5 w-3.5" />
          </Link>
        </div>
      </div>
      <div className="workbench-surface p-4 text-xs leading-relaxed">
        <div className="section-label mb-1.5">{t('admin.mcpParityTitle')}</div>
        <p className="text-muted-foreground">{t('admin.mcpParityDesc')}</p>
      </div>
      <div className="workbench-surface p-4">
        <div className="mb-2 flex items-center justify-between">
          <div>
            <div className="section-label">{t('admin.mcpSystemPromptTitle')}</div>
            <p className="mt-1 text-xs text-muted-foreground">{t('admin.mcpSystemPromptDesc')}</p>
          </div>
          <Button
            variant="outline"
            size="sm"
            disabled={!systemPrompt}
            onClick={() => {
              if (systemPrompt) void navigator.clipboard.writeText(systemPrompt)
            }}
          >
            <Copy className="mr-1.5 h-3.5 w-3.5" /> {t('admin.copy')}
          </Button>
        </div>
        <DataState
          query={{
            isLoading: promptQuery.isLoading,
            error: promptQuery.error,
            data: systemPrompt ?? undefined,
          }}
        >
          {(prompt) => (
            <pre className="max-h-96 overflow-x-auto overflow-y-auto whitespace-pre-wrap rounded-xl bg-surface-sunken p-4 font-mono text-xs leading-relaxed">
              {prompt}
            </pre>
          )}
        </DataState>
      </div>
      <div className="space-y-4">
        {configs.map((cfg) => (
          <div
            key={cfg.name}
            className="workbench-surface overflow-hidden transition-shadow duration-200 hover:shadow-lifted"
          >
            <div className="flex items-center gap-2.5 border-b p-4">
              <div className="flex h-8 w-8 items-center justify-center rounded-xl bg-surface-sunken">
                <cfg.icon className="h-4 w-4 text-muted-foreground" />
              </div>
              <h3 className="text-sm font-bold">{cfg.name}</h3>
            </div>
            <div className="p-4">
              <pre className="overflow-x-auto rounded-xl bg-surface-sunken p-4 font-mono text-xs leading-relaxed">
                {cfg.config}
              </pre>
              <div className="mt-3 flex gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => void navigator.clipboard.writeText(cfg.config)}
                >
                  <Copy className="mr-1.5 h-3.5 w-3.5" /> {t('admin.copy')}
                </Button>
              </div>
            </div>
          </div>
        ))}
      </div>
    </>
  )
}
