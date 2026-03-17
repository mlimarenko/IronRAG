import { configure } from 'quasar/wrappers'

export default configure(function () {
  return {
    supportTS: true,
    boot: ['axios', 'pinia', 'i18n'],
    css: ['app.scss'],
    build: {
      vueRouterMode: 'history',
      extendViteConf(viteConf) {
        viteConf.build ??= {}
        viteConf.build.rollupOptions ??= {}

        if (Array.isArray(viteConf.build.rollupOptions.output)) {
          return
        }

        viteConf.build.rollupOptions.output ??= {}
        viteConf.build.rollupOptions.output.manualChunks = {
          graph: [
            'sigma',
            'graphology',
            '@sigma/node-border',
            '@sigma/edge-curve',
          ],
          ai: [
            'ai',
            '@ai-sdk/vue',
            'ai-elements-vue',
          ],
        }
      },
    },
  }
})
