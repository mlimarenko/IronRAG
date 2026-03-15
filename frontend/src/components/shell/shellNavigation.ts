export type ShellSection = 'files' | 'search' | 'processing' | 'graph' | 'api'

export interface ShellNavItem {
  key: ShellSection
  to: string
  legacyTo?: string
  step: string
  stage: 'primary' | 'advanced'
  emphasis?: 'primary' | 'secondary'
}

export const shellNavItems: readonly ShellNavItem[] = [
  {
    key: 'files',
    to: '/files',
    legacyTo: '/ingest',
    step: '01',
    stage: 'primary',
    emphasis: 'primary',
  },
  {
    key: 'search',
    to: '/search',
    legacyTo: '/ask',
    step: '02',
    stage: 'primary',
    emphasis: 'primary',
  },
  {
    key: 'processing',
    to: '/processing',
    legacyTo: '/setup',
    step: 'A1',
    stage: 'advanced',
    emphasis: 'secondary',
  },
  {
    key: 'graph',
    to: '/graph',
    step: 'A2',
    stage: 'advanced',
    emphasis: 'secondary',
  },
  {
    key: 'api',
    to: '/api',
    step: 'A3',
    stage: 'advanced',
    emphasis: 'secondary',
  },
] as const

export function getShellNavItem(section: ShellSection): ShellNavItem {
  return shellNavItems.find((item) => item.key === section) ?? shellNavItems[0]
}

export function getShellNavIndex(section: ShellSection): number {
  return shellNavItems.findIndex((item) => item.key === section)
}
