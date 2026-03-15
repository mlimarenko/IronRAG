import type { InventoryRow } from 'src/components/design-system/ComponentInventoryTable.vue'

export interface ColorToken {
  label: string
  token: string
  value: string
  textColor?: string
}

export const colorTokens: ColorToken[] = [
  { label: 'Canvas', token: '--rr-color-bg-canvas', value: '#f3f4ee' },
  { label: 'Surface', token: '--rr-surface-panel', value: 'rgb(255 255 255 / 0.92)' },
  { label: 'Accent', token: '--rr-color-accent-600', value: '#1d4ed8', textColor: '#f8fafc' },
  { label: 'Info', token: '--rr-surface-banner-info', value: '#eaf0ff' },
  { label: 'Success', token: '--rr-color-success-600', value: '#15803d', textColor: '#f8fafc' },
  { label: 'Warning', token: '--rr-color-warning-600', value: '#b45309', textColor: '#f8fafc' },
  { label: 'Danger', token: '--rr-color-danger-600', value: '#c2410c', textColor: '#f8fafc' },
] as const

export const spacingTokens = [
  '--rr-space-1 · 4px',
  '--rr-space-2 · 8px',
  '--rr-space-4 · 16px',
  '--rr-space-6 · 24px',
  '--rr-space-7 · 32px',
  '--rr-space-8 · 40px',
]

export const principles = [
  'Operator-first, not marketing-first: dense enough for real work, never cramped.',
  'Foundation starts with semantic tokens, then flows into components and utility classes.',
  'States are first-class: loading, empty, degraded, blocked, and success deserve reusable patterns.',
  'One visual language across shell and product pages: same header, surface, form, and banner rules.',
]

export const workflowSteps = [
  'Use src/pages/DesignSystemPage.vue as a living reference route while Storybook is absent.',
  'Promote page patterns into isolated primitives under src/components/design-system and src/components/state.',
  'When the inventory stabilizes, add Vitest snapshot coverage for token-driven variants.',
  'If the team wants full Storybook later, map each primitive to CSF stories without redesigning the tokens.',
]

export const inventoryRows: InventoryRow[] = [
  {
    primitive: 'App shell + topbar + sidebar',
    purpose: 'Persistent navigation and cross-page operational context.',
    states: 'default, compact/mobile, environment degraded, navigation active',
    nextStep: 'Move route meta and environment status wiring into a typed shell config.',
  },
  {
    primitive: 'Page header + section container',
    purpose:
      'Shared page header and body framing for title, eyebrow, description, actions, and status.',
    states: 'default, with actions, status badge, long description, stacked mobile layout',
    nextStep:
      'Keep top-level pages on the same component instead of reintroducing scoped header CSS.',
  },
  {
    primitive: 'Status badge',
    purpose: 'Compact health/readiness indicator for jobs, providers, and diagnostics.',
    states: 'neutral, info, success, warning, danger; subtle vs strong emphasis',
    nextStep: 'Replace raw status text in dashboard, projects, providers, ingestion.',
  },
  {
    primitive: 'State cards + status banner',
    purpose: 'Consistent empty, loading, and error communication.',
    states: 'empty, loading, error, warning banner, info banner, retry action, secondary hint',
    nextStep:
      'Use in all async pages before adding more bespoke placeholders or inline status text.',
  },
  {
    primitive: 'Panel/card',
    purpose: 'Base surface for metrics, lists, forms, and diagnostics blocks.',
    states: 'default, muted, highlighted, headerless, with actions',
    nextStep: 'Standardize future surface work on the AppPanel wrapper plus rr-panel classes.',
  },
  {
    primitive: 'Buttons + form rows',
    purpose: 'Shared controls for actions and dense forms.',
    states: 'primary, secondary, ghost, disabled, one-column, two-column, three-column',
    nextStep: 'Keep new forms inside rr-form-row or rr-form-grid to prevent ad hoc spacing drift.',
  },
]
