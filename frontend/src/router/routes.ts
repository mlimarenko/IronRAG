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
              workspaceLabel: 'Default workspace',
              projectLabel: 'Portfolio view',
              environmentLabel: 'Shell online',
              environmentStatus: 'Healthy',
            },
          },
          {
            path: 'workspaces',
            component: () => import('src/pages/WorkspacesPage.vue'),
            meta: {
              workspaceLabel: 'All workspaces',
              projectLabel: 'Governance overview',
              environmentLabel: 'Workspace routing ready',
              environmentStatus: 'Healthy',
            },
          },
          {
            path: 'projects',
            component: () => import('src/pages/ProjectsPage.vue'),
            meta: {
              workspaceLabel: 'Default workspace',
              projectLabel: 'Project readiness',
              environmentLabel: 'Project surface wired',
              environmentStatus: 'Healthy',
            },
          },
          {
            path: 'providers',
            component: () => import('src/pages/ProvidersPage.vue'),
            meta: {
              workspaceLabel: 'Default workspace',
              projectLabel: 'Provider governance',
              environmentLabel: 'Admin flow visible',
              environmentStatus: 'Healthy',
            },
          },
          {
            path: 'ingestion',
            component: () => import('src/pages/IngestionPage.vue'),
            meta: {
              workspaceLabel: 'Default workspace',
              projectLabel: 'Ingestion jobs',
              environmentLabel: 'Job tracking ready',
              environmentStatus: 'Degraded',
            },
          },
          {
            path: 'chat',
            component: () => import('src/pages/ChatPage.vue'),
            meta: {
              workspaceLabel: 'Default workspace',
              projectLabel: 'Grounded query workspace',
              environmentLabel: 'Retrieval surface wired',
              environmentStatus: 'Healthy',
            },
          },
        ],
      },
    ],
  },
]

export default routes
