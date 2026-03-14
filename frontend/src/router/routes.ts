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
              shellSection: 'overview',
              shellStatus: 'focused',
            },
          },
          {
            path: 'setup',
            component: () => import('src/pages/WorkspacesPage.vue'),
            meta: {
              shellSection: 'workspace',
              shellStatus: 'ready',
            },
          },
          {
            path: 'ingest',
            component: () => import('src/pages/IngestionPage.vue'),
            meta: {
              shellSection: 'library',
              shellStatus: 'ready',
            },
          },
          {
            path: 'ask',
            component: () => import('src/pages/ChatPage.vue'),
            meta: {
              shellSection: 'search',
              shellStatus: 'healthy',
            },
          },
        ],
      },
    ],
  },
]

export default routes
