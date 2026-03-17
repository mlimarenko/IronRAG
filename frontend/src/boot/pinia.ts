import { boot } from 'quasar/wrappers'
import { pinia } from 'src/lib/pinia'

export default boot(({ app }) => {
  app.use(pinia)
})
