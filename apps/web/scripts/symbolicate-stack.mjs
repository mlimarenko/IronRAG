#!/usr/bin/env node
/**
 * Symbolicate a browser stack trace with the source maps produced by the
 * frontend build. Source maps are release artifacts; they are not served by the
 * web image.
 *
 * Usage:
 *   node scripts/symbolicate-stack.mjs --dist <dir> [stack-file]
 *   node scripts/symbolicate-stack.mjs --dist <dir> --verify
 *
 * Stack frames pointing at /assets/<chunk>.js:line:column are resolved through
 * <dir>/assets/<chunk>.js.map. Other lines are printed unchanged.
 */
import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs'
import path from 'node:path'

import { SourceMapConsumer } from 'source-map-js'

const assetPattern = /(?:https?:\/\/[^\s"'()]+)?\/assets\/([^/\s:()]+\.js):(\d+):(\d+)/g

function parseArgs(argv) {
  let dist = ''
  let verify = false
  const files = []
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index]
    if (argument === '--dist') {
      dist = argv[index + 1] ?? ''
      index += 1
    } else if (argument === '--verify') {
      verify = true
    } else {
      files.push(argument)
    }
  }
  if (!dist) throw new Error('--dist is required')
  return { dist, verify, files }
}

function mapConsumers(dist) {
  const assetsDir = path.join(dist, 'assets')
  if (!existsSync(assetsDir)) throw new Error(`missing assets directory: ${assetsDir}`)
  const consumers = new Map()
  let chunkCount = 0
  for (const entry of readdirSync(assetsDir)) {
    if (!entry.endsWith('.js')) continue
    chunkCount += 1
    const mapPath = path.join(assetsDir, `${entry}.map`)
    // Pure re-export facades and runtime trampolines have no transform to map.
    if (!existsSync(mapPath)) continue
    consumers.set(entry, new SourceMapConsumer(JSON.parse(readFileSync(mapPath, 'utf8'))))
  }
  return { consumers, chunkCount }
}

function verify(consumers, chunkCount) {
  if (consumers.size === 0) throw new Error('no source maps found')
  let applicationChunks = 0
  let mappedFrames = 0
  for (const [, consumer] of consumers) {
    let sourceEntries = 0
    consumer.eachMapping((mapping) => {
      if (mapping.source?.includes('/src/') || mapping.source?.startsWith('src/')) {
        sourceEntries += 1
      }
    })
    // Vendor facades and generated re-export trampolines have a map but no
    // application source of their own; they are not symbolication targets.
    if (sourceEntries === 0) continue
    applicationChunks += 1
    mappedFrames += sourceEntries
  }
  const required = Math.min(10, consumers.size)
  if (applicationChunks < required) {
    throw new Error(
      `only ${applicationChunks} application chunks have source maps (need ${required})`,
    )
  }
  console.log(
    `source maps verified: ${applicationChunks}/${chunkCount} application chunks, ${mappedFrames} mappings resolved to application source`,
  )
}

function symbolicate(input, consumers) {
  return input.replace(assetPattern, (match, chunk, line, column) => {
    const consumer = consumers.get(chunk)
    if (!consumer) return match
    const original = consumer.originalPositionFor({
      line: Number(line),
      column: Math.max(0, Number(column) - 1),
    })
    if (!original.source) return match
    const name = original.name ? ` (${original.name})` : ''
    const source = original.source.replace(/^(?:\.\.\/)+/, '')
    const sourceLine = original.line ?? line
    const sourceColumn = original.column ?? column
    return `${source}:${sourceLine}:${sourceColumn}${name}`
  })
}

const { dist, verify: verifyOnly, files } = parseArgs(process.argv.slice(2))
if (!statSync(dist).isDirectory()) throw new Error(`not a directory: ${dist}`)
const { consumers, chunkCount } = mapConsumers(dist)

if (verifyOnly) {
  verify(consumers, chunkCount)
} else {
  const stack =
    files.length > 0
      ? files.map((file) => readFileSync(file, 'utf8')).join('\n')
      : readFileSync(0, 'utf8')
  process.stdout.write(symbolicate(stack, consumers))
}
