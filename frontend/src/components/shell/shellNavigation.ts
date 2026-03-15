export type ShellSection = 'processing' | 'files' | 'search' | 'graph' | 'api'

export interface ShellNavItem {
  key: ShellSection
  to: string
  legacyTo?: string
  step: string
  stage: 'flow' | 'inspect' | 'extend'
}

export const shellNavItems: readonly ShellNavItem[] = [
  {
    key: 'processing',
    to: '/processing',
    legacyTo: '/setup',
    step: '01',
    stage: 'flow',
  },
  {
    key: 'files',
    to: '/files',
    legacyTo: '/ingest',
    step: '02',
    stage: 'flow',
  },
  {
    key: 'search',
    to: '/search',
    legacyTo: '/ask',
    step: '03',
    stage: 'flow',
  },
  {
    key: 'graph',
    to: '/graph',
    step: '04',
    stage: 'inspect',
  },
  {
    key: 'api',
    to: '/api',
    step: '05',
    stage: 'extend',
  },
] as const

export function getShellNavItem(section: ShellSection): ShellNavItem {
  return shellNavItems.find((item) => item.key === section) ?? shellNavItems[0]
}

export function getShellNavIndex(section: ShellSection): number {
  return shellNavItems.findIndex((item) => item.key === section)
}
