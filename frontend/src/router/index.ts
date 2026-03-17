import { createRouter, createWebHistory } from 'vue-router'
import { installRouteGuards } from './guards'
import routes from './routes'

const router = createRouter({
  history: createWebHistory(),
  routes,
})

installRouteGuards(router)

export default router
