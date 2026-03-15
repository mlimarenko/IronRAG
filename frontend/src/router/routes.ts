const productAliases = [
  {
    path: 'setup',
    redirect: '/processing',
  },
  {
    path: 'ingest',
    redirect: '/files',
  },
  {
    path: 'ask',
    redirect: '/search',
  },
  {
    path: 'chat',
    redirect: '/search',
  },
  {
    path: 'dashboard',
    redirect: '/files',
  },
  {
    path: 'home',
    redirect: '/files',
  },
]

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
            redirect: '/files',
          },
          {
            path: 'processing',
            component: () => import('src/pages/WorkspacesPage.vue'),
            meta: {
              shellSection: 'processing',
              shellStatus: 'focused',
            },
          },
          ...productAliases,
          {
            path: 'files',
            component: () => import('src/pages/IngestionPage.vue'),
            meta: {
              shellSection: 'files',
              shellStatus: 'ready',
            },
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
