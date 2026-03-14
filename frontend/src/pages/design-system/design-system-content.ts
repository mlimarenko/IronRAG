import type { InventoryRow } from 'src/components/design-system/ComponentInventoryTable.vue'

export interface ColorToken {
  label: string
  token: string
  value: string
  textColor?: string
}

export const colorTokens: ColorToken[] = [
  { label: 'Canvas', token: '--rr-color-bg-canvas', value: '#f3f7fb' },
  { label: 'Surface', token: '--rr-color-bg-surface', value: 'rgb(255 255 255 / 0.94)' },
  { label: 'Accent', token: '--rr-color-accent-600', value: '#2563eb', textColor: '#f8fafc' },
  { label: 'Info', token: '--rr-color-info-600', value: '#2563eb', textColor: '#f8fafc' },
  { label: 'Success', token: '--rr-color-success-600', value: '#16a34a', textColor: '#f8fafc' },
  { label: 'Warning', token: '--rr-color-warning-600', value: '#d97706', textColor: '#f8fafc' },
  { label: 'Danger', token: '--rr-color-danger-600', value: '#dc2626', textColor: '#f8fafc' },
] as const

export const spacingTokens = [
  '--rr-space-2 · 8px',
  '--rr-space-4 · 16px',
  '--rr-space-6 · 24px',
  '--rr-space-7 · 32px',
  '--rr-space-8 · 40px',
]

export const principles = [
  'Operator-first, not marketing-first: dense enough for real work, never cramped.',
  'Context stays sticky: workspace, project, ingestion posture, and API status should remain visible.',
  'States are first-class: loading, empty, degraded, blocked, and success deserve reusable patterns.',
  'One visual language across shell and product pages: same tokens, same elevation, same spacing rhythm.',
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
    primitive: 'Page section',
    purpose: 'Shared page header for title, eyebrow, description, and actions.',
    states: 'default, with actions, status badge, long description',
    nextStep: 'Adopt across all top-level pages to kill hand-rolled headers.',
  },
  {
    primitive: 'Status badge',
    purpose: 'Compact health/readiness indicator for jobs, providers, and diagnostics.',
    states: 'neutral, info, success, warning, danger; subtle vs strong emphasis',
    nextStep: 'Replace raw status text in dashboard, projects, providers, ingestion.',
  },
  {
    primitive: 'State cards',
    purpose: 'Consistent empty, loading, and error communication.',
    states: 'empty, loading, error, retry action, secondary hint',
    nextStep: 'Use in all async pages before adding more bespoke placeholders.',
  },
  {
    primitive: 'Panel/card',
    purpose: 'Base surface for metrics, lists, forms, and diagnostics blocks.',
    states: 'default, muted, highlighted, interactive',
    nextStep: 'Extract as a Vue primitive once 2-3 pages share the same markup.',
  },
  {
    primitive: 'Action controls',
    purpose: 'Primary/secondary/destructive button and segmented actions.',
    states: 'default, hover, focus, disabled, busy',
    nextStep: 'Introduce tokenized button classes before forms spread further.',
  },
]
