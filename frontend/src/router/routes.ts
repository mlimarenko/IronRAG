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
            redirect: '/processing',
          },
          {
            path: 'processing',
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
              shellSection: 'setup',
              shellStatus: 'ready',
            },
          },
          {
            path: 'files',
            component: () => import('src/pages/IngestionPage.vue'),
            meta: {
              shellSection: 'files',
              shellStatus: 'ready',
            },
          },
          {
            path: 'ingest',
            redirect: '/files',
          },
          {
            path: 'search',
            component: () => import('src/pages/ChatPage.vue'),
            meta: {
              shellSection: 'search',
              shellStatus: 'healthy',
            },
          },
          {
            path: 'ask',
            redirect: '/search',
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
