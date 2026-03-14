const routes = [
  {
    path: '/',
    component: () => import('src/layouts/AppLayout.vue'),
    children: [
      {
        path: '',
        component: () => import('src/components/shell/AppShell.vue'),
        children: [
          {
            path: '',
            component: () => import('src/pages/DashboardPage.vue'),
            meta: {
              shellSection: 'processing',
              shellStatus: 'focused',
            },
          },
          {
            path: 'setup',
            component: () => import('src/pages/WorkspacesPage.vue'),
            meta: {
              shellSection: 'context',
              shellStatus: 'ready',
            },
          },
          {
            path: 'ingest',
            component: () => import('src/pages/IngestionPage.vue'),
            meta: {
              shellSection: 'files',
              shellStatus: 'ready',
            },
          },
          {
            path: 'ask',
            component: () => import('src/pages/ChatPage.vue'),
            meta: {
              shellSection: 'ask',
              shellStatus: 'healthy',
            },
          },
          {
            path: 'graph',
            component: () => import('src/pages/GraphPage.vue'),
            meta: {
              shellSection: 'graph',
              shellStatus: 'ready',
            },
          },
          {
            path: 'api',
            component: () => import('src/pages/ApiIntegrationsPage.vue'),
            meta: {
              shellSection: 'api',
              shellStatus: 'healthy',
            },
          },
        ],
      },
    ],
  },
]

export default routes
