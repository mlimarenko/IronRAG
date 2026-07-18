declare global {
  var __zod_globalConfig: { jitless?: boolean } | undefined
}

globalThis.__zod_globalConfig = globalThis.__zod_globalConfig
  ? { ...globalThis.__zod_globalConfig, jitless: true }
  : { jitless: true }

export {}
