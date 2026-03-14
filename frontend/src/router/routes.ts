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
              workspaceLabel: 'Overview',
              projectLabel: 'Minimal usable flow',
              environmentLabel: 'Create → ingest → ask',
              environmentStatus: 'Focused',
            },
          },
          {
            path: 'setup',
            component: () => import('src/pages/WorkspacesPage.vue'),
            meta: {
              workspaceLabel: 'Setup',
              projectLabel: 'Workspace and project selection',
              environmentLabel: 'Prepare a working RAG project',
              environmentStatus: 'Ready',
            },
          },
          {
            path: 'ingest',
            component: () => import('src/pages/IngestionPage.vue'),
            meta: {
              workspaceLabel: 'Ingest',
              projectLabel: 'Text and document indexing',
              environmentLabel: 'Populate the knowledge base',
              environmentStatus: 'Ready',
            },
          },
          {
            path: 'ask',
            component: () => import('src/pages/ChatPage.vue'),
            meta: {
              workspaceLabel: 'Ask',
              projectLabel: 'Grounded query flow',
              environmentLabel: 'Query indexed content',
              environmentStatus: 'Ready',
            },
          },
        ],
      },
    ],
  },
]

export default routes
