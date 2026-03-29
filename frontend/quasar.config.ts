import { configure } from 'quasar/wrappers'

export default configure(function () {
  const backendOrigin = (process.env.VITE_BACKEND_URL ?? 'http://127.0.0.1:8095').replace(
    /\/$/,
    '',
  )

  return {
    supportTS: true,
    boot: ['axios', 'pinia', 'i18n'],
    css: ['app.scss'],
    devServer: {
      open: false,
      proxy: {
        '/v1': {
          target: backendOrigin,
          changeOrigin: true,
        },
      },
    },
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
        }
      },
    },
  }
})
