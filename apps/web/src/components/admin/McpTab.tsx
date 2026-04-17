import { useEffect, useState } from 'react';
import type { TFunction } from 'i18next';
import { Copy, Loader2, Terminal, Code2, Brain } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { queryApi } from '@/api';

type McpTabProps = {
  t: TFunction;
  activeLibraryId: string | undefined;
  active: boolean;
};

type McpClientConfig = {
  name: string;
  icon: typeof Terminal;
  config: string;
};

// All snippets assume the MCP Streamable HTTP transport (spec 2025-06-18)
// that IronRAG now speaks natively. No stdio proxy, no bespoke SSE
// endpoint — just the canonical `POST/GET/DELETE /v1/mcp` URL plus a
// bearer token. `${IRONRAG_MCP_TOKEN}` placeholder reminds operators to
// store the token in an env var, not inline in their dotfile.
function getMcpConfigs(origin: string): McpClientConfig[] {
  const mcpUrl = `${origin}/v1/mcp`;
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
  ];
}

export function McpTab({ t, activeLibraryId, active }: McpTabProps) {
  const [systemPrompt, setSystemPrompt] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    void (async () => {
      setLoading(true);
      setError(null);
      try {
        const response = await queryApi.getAssistantSystemPrompt(activeLibraryId);
        if (cancelled) return;
        setSystemPrompt(response.rendered ?? response.template);
      } catch (err: unknown) {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [active, activeLibraryId]);

  const origin = window.location.origin;
  const configs = getMcpConfigs(origin);

  return (
    <>
      <div className="mb-5">
        <h2 className="text-base font-bold tracking-tight">{t('admin.mcpTitle')}</h2>
        <p className="text-sm text-muted-foreground mt-1">{t('admin.mcpDesc')}</p>
      </div>
      <div className="grid grid-cols-2 gap-3 mb-4 text-xs">
        <div className="workbench-surface p-4">
          <div className="section-label mb-1.5">{t('admin.mcpServerUrl')}</div>
          <code className="font-mono text-xs font-bold">{origin}/v1/mcp</code>
        </div>
        <div className="workbench-surface p-4">
          <div className="section-label mb-1.5">{t('admin.capabilitiesProbe')}</div>
          <code className="font-mono text-xs font-bold">{origin}/v1/mcp/capabilities</code>
        </div>
      </div>
      <div className="workbench-surface p-4 mb-6 text-xs leading-relaxed">
        <div className="section-label mb-1.5">{t('admin.mcpParityTitle')}</div>
        <p className="text-muted-foreground">{t('admin.mcpParityDesc')}</p>
      </div>
      <div className="workbench-surface p-4 mb-4">
        <div className="flex items-center justify-between mb-2">
          <div>
            <div className="section-label">{t('admin.mcpSystemPromptTitle')}</div>
            <p className="text-xs text-muted-foreground mt-1">{t('admin.mcpSystemPromptDesc')}</p>
          </div>
          <Button
            variant="outline"
            size="sm"
            disabled={!systemPrompt}
            onClick={() => {
              if (systemPrompt) navigator.clipboard.writeText(systemPrompt);
            }}
          >
            <Copy className="h-3 w-3 mr-1.5" /> {t('admin.copy')}
          </Button>
        </div>
        {loading && (
          <div className="text-xs text-muted-foreground py-4">
            <Loader2 className="h-3 w-3 mr-1.5 inline animate-spin" />
            {t('admin.loading')}
          </div>
        )}
        {error && <div className="text-xs text-destructive py-2">{error}</div>}
        {systemPrompt && !loading && (
          <pre className="text-xs bg-surface-sunken p-4 rounded-xl overflow-x-auto overflow-y-auto max-h-96 font-mono leading-relaxed border border-border/50 whitespace-pre-wrap">
            {systemPrompt}
          </pre>
        )}
      </div>
      <div className="space-y-4">
        {configs.map((cfg) => (
          <div
            key={cfg.name}
            className="workbench-surface overflow-hidden transition-shadow duration-200 hover:shadow-lifted"
          >
            <div className="flex items-center gap-2.5 p-4 border-b">
              <div className="w-8 h-8 rounded-xl bg-surface-sunken flex items-center justify-center">
                <cfg.icon className="h-4 w-4 text-muted-foreground" />
              </div>
              <h3 className="text-sm font-bold">{cfg.name}</h3>
            </div>
            <div className="p-4">
              <pre className="text-xs bg-surface-sunken p-4 rounded-xl overflow-x-auto font-mono leading-relaxed border border-border/50">
                {cfg.config}
              </pre>
              <div className="flex gap-2 mt-3">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => navigator.clipboard.writeText(cfg.config)}
                >
                  <Copy className="h-3 w-3 mr-1.5" /> {t('admin.copy')}
                </Button>
              </div>
            </div>
          </div>
        ))}
      </div>
    </>
  );
}
