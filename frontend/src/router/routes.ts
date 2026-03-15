const productAliases = [
  {
    path: 'setup',
    redirect: '/documents',
  },
  {
    path: 'processing',
    redirect: '/documents',
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
          {
            path: 'advanced/projects',
            component: () => import('src/pages/ProjectsPage.vue'),
            meta: {
              shellSection: 'advanced',
              shellStatus: 'focused',
            },
          },
          {
            path: 'advanced/providers',
            component: () => import('src/pages/ProvidersPage.vue'),
            meta: {
              shellSection: 'advanced',
              shellStatus: 'focused',
            },
          },
          {
            path: 'advanced/diagnostics',
            component: () => import('src/pages/DiagnosticsPage.vue'),
            meta: {
              shellSection: 'advanced',
              shellStatus: 'warning',
            },
          },
          {
            path: 'advanced/onboarding',
            component: () => import('src/pages/OnboardingPage.vue'),
            meta: {
              shellSection: 'advanced',
              shellStatus: 'focused',
            },
          },
          {
            path: 'advanced/graph',
            component: () => import('src/pages/GraphPage.vue'),
            meta: {
              shellSection: 'advanced',
              shellStatus: 'focused',
            },
          },
          {
            path: 'projects',
            redirect: '/advanced/projects',
          },
          {
            path: 'providers',
            redirect: '/advanced/providers',
          },
          {
            path: 'diagnostics',
            redirect: '/advanced/diagnostics',
          },
          {
            path: 'onboarding',
            redirect: '/advanced/onboarding',
          },
          {
            path: 'graph',
            redirect: '/advanced/graph',
          },
        ],
      },
    ],
  },
]

export default routes
