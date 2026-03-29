<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import 'swagger-ui-dist/swagger-ui.css'
import swaggerBundleUrl from 'swagger-ui-dist/swagger-ui-bundle.js?url'
import swaggerStandalonePresetUrl from 'swagger-ui-dist/swagger-ui-standalone-preset.js?url'
import { resolveApiPath } from 'src/services/api/http'

interface SwaggerUiInstance {
  destroy?: () => void
}

interface SwaggerUiBundle {
  (options: Record<string, unknown>): SwaggerUiInstance
  presets?: {
    apis?: unknown
  }
}

declare global {
  interface Window {
    SwaggerUIBundle?: SwaggerUiBundle
    SwaggerUIStandalonePreset?: unknown
  }
}

const { t } = useI18n()
const swaggerRoot = ref<HTMLElement | null>(null)
const loadError = ref<string | null>(null)
const rawSpecUrl = resolveApiPath('/openapi/rustrag.openapi.yaml')

let swaggerUi: { destroy?: () => void } | null = null
let isUnmounted = false

function loadScript(src: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const existingScript = document.querySelector<HTMLScriptElement>(`script[src="${src}"]`)

    if (existingScript?.dataset.loaded === 'true') {
      resolve()
      return
    }

    if (existingScript) {
      existingScript.addEventListener(
        'load',
        () => {
          resolve()
        },
        { once: true },
      )
      existingScript.addEventListener(
        'error',
        () => {
          reject(new Error(`failed to load ${src}`))
        },
        {
          once: true,
        },
      )
      return
    }

    const script = document.createElement('script')
    script.src = src
    script.async = true
    script.addEventListener(
      'load',
      () => {
        script.dataset.loaded = 'true'
        resolve()
      },
      { once: true },
    )
    script.addEventListener(
      'error',
      () => {
        reject(new Error(`failed to load ${src}`))
      },
      { once: true },
    )
    document.head.appendChild(script)
  })
}

async function ensureSwaggerUiBundle(): Promise<SwaggerUiBundle> {
  if (!window.SwaggerUIBundle) {
    await loadScript(swaggerBundleUrl)
  }

  if (!window.SwaggerUIStandalonePreset) {
    await loadScript(swaggerStandalonePresetUrl)
  }

  if (!window.SwaggerUIBundle) {
    throw new Error('swagger bundle did not initialize')
  }

  return window.SwaggerUIBundle
}

onMounted(() => {
  void (async () => {
    try {
      const bundle = await ensureSwaggerUiBundle()

      if (isUnmounted || !swaggerRoot.value) {
        return
      }

      const presets = [bundle.presets?.apis, window.SwaggerUIStandalonePreset].filter(
        (preset): preset is NonNullable<typeof preset> => preset != null,
      )

      swaggerUi = bundle({
        domNode: swaggerRoot.value,
        url: rawSpecUrl,
        deepLinking: true,
        displayRequestDuration: true,
        docExpansion: 'list',
        defaultModelsExpandDepth: -1,
        persistAuthorization: true,
        presets,
      })
    } catch (error) {
      loadError.value = error instanceof Error ? error.message : String(error)
    }
  })()
})

onBeforeUnmount(() => {
  isUnmounted = true
  swaggerUi?.destroy?.()
  swaggerUi = null
})
</script>

<template>
  <section class="rr-swagger-page">
    <header class="rr-page-card rr-swagger-page__hero">
      <div class="rr-swagger-page__hero-copy">
        <p class="rr-swagger-page__eyebrow">Swagger</p>
        <h1>{{ t('swagger.title') }}</h1>
        <p>{{ t('swagger.subtitle') }}</p>
      </div>
      <a
        class="rr-swagger-page__raw-link"
        :href="rawSpecUrl"
        target="_blank"
        rel="noreferrer"
      >
        {{ t('swagger.rawSpec') }}
      </a>
    </header>

    <section class="rr-page-card rr-swagger-page__frame">
      <p
        v-if="loadError"
        class="rr-swagger-page__error"
      >
        {{ loadError }}
      </p>
      <div
        ref="swaggerRoot"
        class="rr-swagger-page__ui"
      />
    </section>
  </section>
</template>
