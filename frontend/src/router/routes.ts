const routes = [
  {
    path: '/',
    component: () => import('src/layouts/AppLayout.vue'),
    children: [
      { path: '', component: () => import('src/pages/DashboardPage.vue') },
      { path: 'workspaces', component: () => import('src/pages/WorkspacesPage.vue') },
      { path: 'projects', component: () => import('src/pages/ProjectsPage.vue') },
      { path: 'providers', component: () => import('src/pages/ProvidersPage.vue') },
      { path: 'ingestion', component: () => import('src/pages/IngestionPage.vue') },
      { path: 'chat', component: () => import('src/pages/ChatPage.vue') },
      { path: 'diagnostics', component: () => import('src/pages/DiagnosticsPage.vue') },
    ],
  },
]

export default routes
