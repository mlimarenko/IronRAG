import { defineConfig } from '@hey-api/openapi-ts'

export default defineConfig({
  input: '../api/contracts/openapi.gen.yaml',
  output: {
    path: 'src/shared/api/generated',
    postProcess: [
      {
        command: 'node',
        args: ['scripts/normalize-generated-sdk.mjs', '{{path}}'],
      },
    ],
  },
  plugins: [
    '@hey-api/typescript',
    {
      name: '@hey-api/sdk',
      operations: { strategy: 'byTags' },
    },
    {
      name: '@hey-api/client-fetch',
      runtimeConfigPath: './src/shared/api/runtime',
    },
    '@tanstack/react-query',
  ],
})
