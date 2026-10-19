import { describe, expect, it } from 'vitest'
import { getMcpConfigs } from './mcpConfigs'

describe('getMcpConfigs', () => {
  it('returns all seven production client snippets for the current origin', () => {
    const configs = getMcpConfigs('https://ironrag.example.test')
    expect(configs).toHaveLength(7)
    expect(configs.map((config) => config.name)).toEqual([
      'Claude Code',
      'Claude Desktop',
      'Cursor',
      'Codex',
      'VS Code',
      'OpenClaw',
      'Hermes',
    ])
    expect(
      configs.every((config) => config.config.includes('https://ironrag.example.test/v1/mcp')),
    ).toBe(true)
  })
})
