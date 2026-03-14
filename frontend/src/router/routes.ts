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
            path: 'onboarding',
            component: () => import('src/pages/OnboardingPage.vue'),
            meta: {
              workspaceLabel: 'Getting started',
              projectLabel: 'Operator onboarding',
              environmentLabel: 'Guided setup flow',
              environmentStatus: 'In progress',
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
          {
            path: 'graph',
            component: () => import('src/pages/GraphPage.vue'),
            meta: {
              workspaceLabel: 'Default workspace',
              projectLabel: 'Graph coverage workspace',
              environmentLabel: 'Graph evidence preview',
              environmentStatus: 'In progress',
            },
          },
          {
            path: 'api',
            component: () => import('src/pages/ApiIntegrationsPage.vue'),
            meta: {
              workspaceLabel: 'Default workspace',
              projectLabel: 'API integrations',
              environmentLabel: 'Integration surface ready',
              environmentStatus: 'Healthy',
            },
          },
          {
            path: 'diagnostics',
            component: () => import('src/pages/DiagnosticsPage.vue'),
            meta: {
              workspaceLabel: 'Instance diagnostics',
              projectLabel: 'Remediation queue',
              environmentLabel: 'Monitoring for incidents',
              environmentStatus: 'Warning',
            },
          },
          {
            path: 'design-system',
            component: () => import('src/pages/DesignSystemPage.vue'),
            meta: {
              workspaceLabel: 'Frontend foundations',
              projectLabel: 'Design system proposal',
              environmentLabel: 'Reference route active',
              environmentStatus: 'Draft',
            },
          },
        ],
      },
    ],
  },
]

export default routes
