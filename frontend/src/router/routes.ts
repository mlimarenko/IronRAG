const routes = [
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
        path: '',
        name: 'home',
        component: () => import('src/pages/DashboardPage.vue'),
        meta: { title: 'Home', widthMode: 'default' },
      },
      {
        path: 'documents',
        name: 'documents',
        component: () => import('src/pages/DocumentsPage.vue'),
        meta: { title: 'Documents', widthMode: 'wide' },
      },
      {
        path: 'graph',
        name: 'graph',
        component: () => import('src/pages/GraphPage.vue'),
        meta: { title: 'Graph', widthMode: 'full' },
      },
      {
        path: 'admin',
        name: 'admin',
        component: () => import('src/pages/AdminPage.vue'),
        meta: { title: 'Admin', widthMode: 'default', requiresAdmin: true },
      },
      {
        path: 'swagger',
        name: 'swagger',
        component: () => import('src/pages/SwaggerPage.vue'),
        meta: { title: 'API Reference', widthMode: 'wide' },
      },
    ],
  },
  {
    path: '/:pathMatch(.*)*',
    redirect: '/',
  },
]

export default routes
