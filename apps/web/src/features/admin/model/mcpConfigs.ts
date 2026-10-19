export type McpClientConfig = Readonly<{ name: string; config: string }>

export function getMcpConfigs(origin: string): McpClientConfig[] {
  const mcpUrl = `${origin}/v1/mcp`
  return [
    {
      name: 'Claude Code',
      config: `claude mcp add ironrag ${mcpUrl} \\\n+  --transport http \\\n+  --header "Authorization: Bearer $IRONRAG_MCP_TOKEN"`,
    },
    {
      name: 'Claude Desktop',
      config: `{\n  "mcpServers": {\n    "ironrag": {\n      "url": "${mcpUrl}",\n      "headers": {\n        "Authorization": "Bearer \${IRONRAG_MCP_TOKEN}"\n      }\n    }\n  }\n}`,
    },
    {
      name: 'Cursor',
      config: `// .cursor/mcp.json\n{\n  "mcpServers": {\n    "ironrag": {\n      "url": "${mcpUrl}",\n      "headers": {\n        "Authorization": "Bearer \${env:IRONRAG_MCP_TOKEN}"\n      }\n    }\n  }\n}`,
    },
    {
      name: 'Codex',
      config: `# ~/.codex/config.toml\n[mcp_servers.ironrag]\nurl = "${mcpUrl}"\nbearer_token_env_var = "IRONRAG_MCP_TOKEN"`,
    },
    {
      name: 'VS Code',
      config: `// .vscode/mcp.json\n{\n  "servers": {\n    "ironrag": {\n      "type": "http",\n      "url": "${mcpUrl}",\n      "headers": {\n        "Authorization": "Bearer \${env:IRONRAG_MCP_TOKEN}"\n      }\n    }\n  }\n}`,
    },
    {
      name: 'OpenClaw',
      config: `openclaw mcp set ironrag '{"url":"${mcpUrl}","headers":{"Authorization":"Bearer $IRONRAG_MCP_TOKEN"}}'`,
    },
    {
      name: 'Hermes',
      config: `// ~/.hermes/mcp.json\n{\n  "mcpServers": {\n    "ironrag": {\n      "url": "${mcpUrl}",\n      "headers": {\n        "Authorization": "Bearer \${IRONRAG_MCP_TOKEN}"\n      }\n    }\n  }\n}`,
    },
  ]
}
