import { configure } from 'quasar/wrappers'

export default configure(function () {
  return {
    supportTS: true,
    boot: ['api'],
    css: ['app.scss'],
    build: {
      vueRouterMode: 'history',
    },
  }
})
