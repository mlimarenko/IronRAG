import path from 'node:path'
import { fileURLToPath } from 'node:url'
import type { StorybookConfig } from '@storybook/react-vite'
import { loadConfigFromFile, mergeConfig, type UserConfig } from 'vite'

const configDir = path.dirname(fileURLToPath(import.meta.url))
const appViteConfigPath = path.resolve(configDir, '../vite.config.ts')

const loadAppViteConfig = async (mode: string): Promise<UserConfig> => {
  const loaded = await loadConfigFromFile({ command: 'serve', mode }, appViteConfigPath)
  return loaded?.config ?? {}
}

const config: StorybookConfig = {
  stories: ['../src/**/*.stories.@(ts|tsx)'],
  staticDirs: ['../public'],
  addons: ['@storybook/addon-docs', '@storybook/addon-a11y', '@storybook/addon-themes'],
  framework: {
    name: '@storybook/react-vite',
    options: {},
  },
  viteFinal: async (storybookConfig, { configType }) => {
    const appConfig = await loadAppViteConfig(
      configType === 'PRODUCTION' ? 'production' : 'development',
    )
    return mergeConfig(mergeConfig(appConfig, storybookConfig), {
      build: {
        minify: false,
      },
    })
  },
}

export default config
