#!/usr/bin/env node

import { createHash } from 'node:crypto'
import { promises as fs } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const require = createRequire(import.meta.url)
const packageRoot = dirname(require.resolve('swagger-ui-dist/package.json'))
const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const publicRoot = join(projectRoot, 'public')
const checkOnly = process.argv.includes('--check')

const stripSourceMapReference = (content) =>
  content.replace(/\n?\/\*# sourceMappingURL=swagger-ui\.css\.map \*\/\s*$/u, '\n')

const assets = [
  {
    source: 'swagger-ui-bundle.js',
    destination: 'swagger-ui-bundle.js',
  },
  {
    source: 'swagger-ui.css',
    destination: 'swagger-ui.css',
    transform: stripSourceMapReference,
  },
  {
    source: 'swagger-ui-bundle.js.LICENSE.txt',
    destination: 'swagger-ui-bundle.js.LICENSE.txt',
  },
  {
    source: 'LICENSE',
    destination: 'swagger-ui.LICENSE.txt',
  },
]

const sha256 = (content) => createHash('sha256').update(content).digest('hex')

async function expectedContent(asset) {
  const source = await fs.readFile(join(packageRoot, asset.source))
  return asset.transform ? Buffer.from(asset.transform(source.toString('utf8'))) : source
}

async function existingContent(path) {
  try {
    return await fs.readFile(path)
  } catch (error) {
    if (error?.code === 'ENOENT') return null
    throw error
  }
}

async function writeAtomically(path, content) {
  const stagingPath = `${path}.tmp-${process.pid}`
  await fs.writeFile(stagingPath, content, { mode: 0o644 })
  await fs.rename(stagingPath, path)
}

const drift = []
for (const asset of assets) {
  const destination = join(publicRoot, asset.destination)
  const expected = await expectedContent(asset)
  const existing = await existingContent(destination)
  if (existing?.equals(expected)) continue

  drift.push({
    destination: asset.destination,
    expected: sha256(expected),
    actual: existing ? sha256(existing) : 'missing',
  })
  if (!checkOnly) await writeAtomically(destination, expected)
}

if (checkOnly && drift.length > 0) {
  for (const item of drift) {
    console.error(
      `${item.destination}: vendored Swagger UI drift (actual=${item.actual}, expected=${item.expected})`,
    )
  }
  console.error('Run `npm run swagger:sync` and commit the generated assets.')
  process.exit(1)
}

if (!checkOnly && drift.length > 0) {
  console.log(`Synchronized ${drift.length} Swagger UI asset(s) from swagger-ui-dist.`)
}
