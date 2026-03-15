const productAliases = [
  {
    path: 'setup',
    redirect: '/advanced/context',
  },
  {
    path: 'processing',
    redirect: '/advanced/context',
  },
  {
    path: 'ingest',
    redirect: '/documents',
  },
  {
    path: 'files',
    redirect: '/documents',
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
    redirect: '/documents',
  },
  {
    path: 'home',
    redirect: '/documents',
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
            redirect: '/documents',
          },
          ...productAliases,
          {
            path: 'documents',
            component: () => import('src/pages/IngestionPage.vue'),
            meta: {
              shellSection: 'documents',
              shellStatus: 'ready',
            },
          },
          {
            path: 'search',
            component: () => import('src/pages/ChatPage.vue'),
            meta: {
              shellSection: 'ask',
              shellStatus: 'healthy',
            },
          },
          {
            path: 'advanced/context',
            component: () => import('src/pages/WorkspacesPage.vue'),
            meta: {
              shellSection: 'advanced',
              shellStatus: 'focused',
            },
          },
          {
            path: 'advanced/api',
            component: () => import('src/pages/ApiIntegrationsPage.vue'),
            meta: {
              shellSection: 'advanced',
              shellStatus: 'healthy',
            },
          },
        ],
      },
    ],
  },
]

export default routes
