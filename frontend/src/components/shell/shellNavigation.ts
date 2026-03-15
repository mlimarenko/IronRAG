export type ShellSection = 'documents' | 'ask' | 'advanced'

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
    key: 'documents',
    to: '/documents',
    legacyTo: '/files',
    step: '01',
    stage: 'primary',
    emphasis: 'primary',
  },
  {
    key: 'ask',
    to: '/search',
    legacyTo: '/ask',
    step: '02',
    stage: 'primary',
    emphasis: 'primary',
  },
  {
    key: 'advanced',
    to: '/advanced/context',
    legacyTo: '/processing',
    step: 'A',
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
