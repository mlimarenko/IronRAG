import { describe, expect, it } from 'vitest'
import { execFile } from 'node:child_process'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { promisify } from 'node:util'

const execFileAsync = promisify(execFile)
const webRoot = path.resolve(import.meta.dirname, '../..')
const normalizer = path.join(webRoot, 'scripts/normalize-generated-sdk.mjs')

const GENERATED_SSE_SOURCE = `export function createSseClient() {
  function parseSseField(state, line) {
    return state;
  }

  function parseSseChunk(chunk, previous) {
    const initial = { dataLines: [], retryDelay: previous.retryDelay };
    const parsed = chunk.split('\\n').reduce(parseSseField, initial);
    return parsed;
  }

  const stream = undefined;
  return { stream };
}
`

const GENERATED_REACT_QUERY_SOURCE = `const createQueryKey = <TOptions extends Options>(id: string, options?: TOptions): [
    QueryKey<TOptions>[0]
] => {
    const params: QueryKey<TOptions>[0] = { _id: id, baseUrl: options?.baseUrl || (options?.client ?? client).getConfig().baseUrl } as QueryKey<TOptions>[0];
    return [params];
};

const createInfiniteParams = <K extends Pick<QueryKey<Options>[0], 'body' | 'headers' | 'path' | 'query'>>(queryKey: QueryKey<Options>, page: K) => {
  return { ...queryKey[0], ...page };
};

const options = infiniteQueryOptions<Response, DefaultError, InfiniteData<Response>, QueryKey<Options<ListData>>, number | Pick<QueryKey<Options<ListData>>[0], 'body' | 'headers' | 'path' | 'query'>>({});
const stringOptions = infiniteQueryOptions<Response, DefaultError, InfiniteData<Response>, QueryKey<Options<ListData>>, string | Pick<QueryKey<Options<ListData>>[0], 'body' | 'headers' | 'path' | 'query'>>({});
const page: Pick<QueryKey<Options<ListAuditEventsData>>[0], 'body' | 'headers' | 'path' | 'query'> = {};
`

async function normalizeFixture(files) {
  const temporaryRoot = await mkdtemp(path.join(tmpdir(), 'ironrag-sdk-'))

  try {
    await Promise.all(
      Object.entries(files).map(async ([relativePath, source]) => {
        const filePath = path.join(temporaryRoot, relativePath)
        await mkdir(path.dirname(filePath), { recursive: true })
        await writeFile(filePath, source, 'utf8')
      }),
    )
    await execFileAsync(process.execPath, [normalizer, temporaryRoot])
    return await Promise.all(
      Object.keys(files).map(async (relativePath) => [
        relativePath,
        await readFile(path.join(temporaryRoot, relativePath), 'utf8'),
      ]),
    )
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true })
  }
}

describe('generated SDK normalization', () => {
  it('normalizes generated SSE parsing with an explicit reducer adapter', async () => {
    const normalized = Object.fromEntries(
      await normalizeFixture({ 'core/serverSentEvents.gen.ts': GENERATED_SSE_SOURCE }),
    )

    expect(normalized).toHaveProperty('core/serverSentEvents.gen.ts')
    expect(normalized['core/serverSentEvents.gen.ts']).toMatch(
      /chunk\.split\('\\n'\)\.reduce\(\s*\(state, line\) => parseSseField\(state, line\),\s*initial,?\s*\)/,
    )
    expect(normalized['core/serverSentEvents.gen.ts']).not.toMatch(
      /chunk\.split\('\\n'\)\.reduce\(parseSseField, initial\)/,
    )
  })

  it('normalizes generated React Query base URL fallback with nullish coalescing', async () => {
    const normalized = Object.fromEntries(
      await normalizeFixture({ '@tanstack/react-query.gen.ts': GENERATED_REACT_QUERY_SOURCE }),
    )

    expect(normalized).toHaveProperty('@tanstack/react-query.gen.ts')
    expect(normalized['@tanstack/react-query.gen.ts']).toMatch(
      /baseUrl: options\?\.baseUrl \?\? \(options\?\.client \?\? client\)\.getConfig\(\)\.baseUrl/,
    )
    expect(normalized['@tanstack/react-query.gen.ts']).not.toMatch(/options\?\.baseUrl \|\|/)
    expect(normalized['@tanstack/react-query.gen.ts']).toMatch(
      /type InfinitePageParam = number \| InfinitePageParams;/,
    )
    expect(normalized['@tanstack/react-query.gen.ts']).toMatch(/InfinitePageParam>/)
    expect(normalized['@tanstack/react-query.gen.ts']).toMatch(
      /GenericInfinitePageParams<ListAuditEventsData>/,
    )
  })
})
