import React from 'react'
import type { Decorator, Preview } from '@storybook/react'
import { withThemeByClassName } from '@storybook/addon-themes'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router-dom'
import { initialize, mswLoader } from 'msw-storybook-addon'
import { handlers } from '../src/shared/api/mocks/handlers'
import '../src/index.css'

initialize({ onUnhandledRequest: 'bypass' })

const createQueryClient = () =>
  new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
        staleTime: Number.POSITIVE_INFINITY,
      },
      mutations: {
        retry: false,
      },
    },
  })

const withQueryClient: Decorator = (Story) =>
  React.createElement(
    QueryClientProvider,
    { client: createQueryClient() },
    React.createElement(Story),
  )

const withRouter: Decorator = (Story) =>
  React.createElement(MemoryRouter, { initialEntries: ['/'] }, React.createElement(Story))

const preview: Preview = {
  decorators: [
    withThemeByClassName({
      themes: {
        light: '',
        dark: 'dark',
      },
      defaultTheme: 'light',
    }),
    withQueryClient,
    withRouter,
  ],
  loaders: [mswLoader],
  parameters: {
    layout: 'centered',
    themes: {
      default: 'light',
      list: [
        { name: 'light', class: '', color: '#ffffff' },
        { name: 'dark', class: 'dark', color: '#020817' },
      ],
    },
    msw: {
      handlers,
    },
    a11y: {
      config: {
        rules: [
          { id: 'aria-hidden-focus', enabled: true },
          { id: 'button-name', enabled: true },
          { id: 'color-contrast', enabled: true },
          { id: 'label', enabled: true },
        ],
      },
      options: {
        runOnly: {
          type: 'tag',
          values: ['wcag2a', 'wcag2aa', 'wcag21aa'],
        },
      },
    },
  },
}

export default preview
