import { configure } from 'quasar/wrappers'

export default configure(function () {
  return {
    supportTS: true,
    boot: ['chunk-recovery', 'api'],
    css: ['app.scss'],
    build: {
      vueRouterMode: 'history',
    },
  }
})
