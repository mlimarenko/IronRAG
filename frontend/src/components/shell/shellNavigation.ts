export type ShellSection = 'processing' | 'files' | 'search' | 'graph' | 'api'

export interface ShellNavItem {
  key: ShellSection
  to: string
  legacyTo?: string
  step: string
  stage: 'flow' | 'inspect' | 'extend'
  emphasis?: 'primary' | 'secondary'
}

export const shellNavItems: readonly ShellNavItem[] = [
  {
    key: 'processing',
    to: '/processing',
    legacyTo: '/setup',
    step: '01',
    stage: 'flow',
    emphasis: 'primary',
  },
  {
    key: 'files',
    to: '/files',
    legacyTo: '/ingest',
    step: '02',
    stage: 'flow',
    emphasis: 'primary',
  },
  {
    key: 'search',
    to: '/search',
    legacyTo: '/ask',
    step: '03',
    stage: 'flow',
    emphasis: 'primary',
  },
  {
    key: 'graph',
    to: '/graph',
    step: '04',
    stage: 'inspect',
    emphasis: 'secondary',
  },
  {
    key: 'api',
    to: '/api',
    step: '05',
    stage: 'extend',
    emphasis: 'secondary',
  },
] as const

export function getShellNavItem(section: ShellSection): ShellNavItem {
  return shellNavItems.find((item) => item.key === section) ?? shellNavItems[0]
}

export function getShellNavIndex(section: ShellSection): number {
  return shellNavItems.findIndex((item) => item.key === section)
}
