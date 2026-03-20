const routes = [
  {
    path: '/',
    redirect: '/documents',
  },
  {
    path: '/login',
    component: () => import('src/pages/LoginPage.vue'),
    meta: { guestOnly: true },
  },
  {
    path: '/',
    component: () => import('src/layouts/AppShellLayout.vue'),
    meta: { requiresAuth: true },
    children: [
      {
        path: 'documents',
        component: () => import('src/pages/DocumentsPage.vue'),
      },
      {
        path: 'graph',
        component: () => import('src/pages/GraphPage.vue'),
      },
      {
        path: 'swagger',
        component: () => import('src/pages/SwaggerPage.vue'),
      },
      {
        path: 'admin',
        component: () => import('src/pages/AdminPage.vue'),
        meta: { requiresAdmin: true },
      },
    ],
  },
  {
    path: '/:pathMatch(.*)*',
    redirect: '/documents',
  },
]

export default routes
