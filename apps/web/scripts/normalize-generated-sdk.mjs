import { readdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'

const outputPath = process.argv[2]

if (!outputPath) {
  throw new Error('generated SDK output path is required')
}

function replaceGenerated(source, generated, compatible, relativePath) {
  if (compatible && source.includes(compatible)) {
    return source
  }
  if (source.includes(generated)) {
    return source.replaceAll(generated, compatible)
  }
  if (!compatible) {
    return source
  }
  throw new Error(`unexpected @hey-api output in ${relativePath}`)
}

function normalizeExactOptionalProperties(relativePath, source) {
  let normalized = source
  if (relativePath === 'client/client.gen.ts') {
    normalized = replaceGenerated(
      normalized,
      "import { createSseClient } from '../core/serverSentEvents.gen';\n",
      "import { createSseClient } from '../core/serverSentEvents.gen';\nimport type { ServerSentEventsOptions } from '../core/serverSentEvents.gen';\n",
      relativePath,
    )
    normalized = replaceGenerated(
      normalized,
      '      serializedBody: getValidRequestBody(opts) as BodyInit | null | undefined,\n      url,\n    });',
      '      serializedBody: getValidRequestBody(opts) as BodyInit | null | undefined,\n      url,\n    } as ServerSentEventsOptions<unknown>);',
      relativePath,
    )
    normalized = normalized.replace(
      '      const result = {\n        request,\n        response,\n      };\n\n',
      '',
    )
  }

  if (relativePath === 'client/utils.gen.ts') {
    normalized = replaceGenerated(
      normalized,
      '    path: options.path,\n    query: options.query,',
      '',
      relativePath,
    )
    normalized = replaceGenerated(
      normalized,
      '    url: options.url,',
      '    url: options.url,\n    ...(options.path !== undefined ? { path: options.path } : {}),\n    ...(options.query !== undefined ? { query: options.query } : {}),',
      relativePath,
    )
  }

  if (relativePath === 'core/params.gen.ts') {
    normalized = replaceGenerated(
      normalized,
      '          map: config.map,',
      '          ...(config.map !== undefined ? { map: config.map } : {}),',
      relativePath,
    )
  }

  if (relativePath === 'core/pathSerializer.gen.ts') {
    normalized = replaceGenerated(
      normalized,
      '        allowReserved,',
      '        ...(allowReserved !== undefined ? { allowReserved } : {}),',
      relativePath,
    )
  }

  return normalized
}

function normalizeGeneratedFindings(relativePath, source) {
  let normalized = source
  if (relativePath === 'client/client.gen.ts') {
    normalized = replaceGenerated(
      normalized,
      '      // TODO: we probably want to return error and improve types\n',
      '',
      relativePath,
    )
  }

  if (relativePath === 'client/utils.gen.ts') {
    normalized = replaceGenerated(
      normalized,
      "  if (cleanContent.startsWith('text/')) {\n    return 'text';\n  }\n\n  return;\n};\n\nconst checkForExistence",
      "  if (cleanContent.startsWith('text/')) {\n    return 'text';\n  }\n};\n\nconst checkForExistence",
      relativePath,
    )
    normalized = replaceGenerated(
      normalized,
      "  if (!name) {\n    return false;\n  }\n  if (\n    options.headers.has(name) ||\n    options.query?.[name] ||\n    options.headers.get('Cookie')?.includes(`${name}=`)\n  ) {\n    return true;\n  }\n  return false;",
      "  return Boolean(\n    name &&\n      (options.headers.has(name) ||\n        options.query?.[name] ||\n        options.headers.get('Cookie')?.includes(`${name}=`))\n  );",
      relativePath,
    )
    normalized = replaceGenerated(
      normalized,
      '        if (!options.query) {\n          options.query = {};\n        }',
      '        options.query ??= {};',
      relativePath,
    )
  }

  if (relativePath === 'core/params.gen.ts') {
    normalized = replaceGenerated(
      normalized,
      '  if (!map) {\n    map = new Map();\n  }',
      '  map ??= new Map();',
      relativePath,
    )
    const writeSlotStart = normalized.indexOf('  function writeSlot(')
    const writeSlotEnd = normalized.indexOf('\n  }\n\n  let config:', writeSlotStart)
    if (writeSlotStart >= 0 && writeSlotEnd >= 0) {
      normalized = `${normalized.slice(0, writeSlotStart)}${normalized.slice(writeSlotEnd + 5)}`
    }
  }

  if (relativePath === 'core/pathSerializer.gen.ts') {
    normalized = replaceGenerated(
      normalized,
      "export const separatorObjectExplode = (style: ObjectSeparatorStyle): '.' | ';' | ',' | '&' => {\n  switch (style) {\n    case 'label':\n      return '.';\n    case 'matrix':\n      return ';';\n    case 'simple':\n      return ',';\n    default:\n      return '&';\n  }\n};",
      "export const separatorObjectExplode = (style: ObjectSeparatorStyle): '.' | ';' | ',' | '&' =>\n  separatorArrayExplode(style as ArraySeparatorStyle);",
      relativePath,
    )
    normalized = replaceGenerated(
      normalized,
      "    throw new Error(\n      'Deeply-nested arrays/objects aren’t supported. Provide your own `querySerializer()` to handle these.',\n    );",
      "    throw new TypeError(\n      'Deeply-nested arrays/objects aren’t supported. Provide your own `querySerializer()` to handle these.',\n    );",
      relativePath,
    )
  }

  if (relativePath === 'core/queryKeySerializer.gen.ts') {
    normalized = replaceGenerated(
      normalized,
      'export const queryKeyJsonReplacer = (_key: string, value: unknown): unknown | undefined => {',
      'export const queryKeyJsonReplacer = (_key: string, value: unknown): unknown => {',
      relativePath,
    )
  }

  return normalized
}

function normalizeClientComplexity(relativePath, source) {
  let normalized = source
  if (relativePath === 'client/client.gen.ts') {
    const generated = "  const request: Client['request'] = async (options) => {"
    const compatible = `  const validateAndTransform = async (data: any, opts: ResolvedRequestOptions) => {
    if (opts.responseValidator) await opts.responseValidator(data);
    return opts.responseTransformer ? opts.responseTransformer(data) : data;
  };

  const emptyResponseData = async (response: Response, parseAs: NonNullable<Exclude<Config['parseAs'], 'auto'>>) => {
    if (parseAs === 'formData') return new FormData();
    if (parseAs === 'stream') return response.body;
    if (parseAs === 'json') return {};
    return response[parseAs]();
  };

  const parseSuccessResponse = async (response: Response, opts: ResolvedRequestOptions, request: Request) => {
    const result = { request, response };
    const parseAs = (opts.parseAs === 'auto' ? getParseAs(response.headers.get('Content-Type')) : opts.parseAs) ?? 'json';
    if (response.status === 204 || response.headers.get('Content-Length') === '0') {
      const emptyData = await emptyResponseData(response, parseAs);
      return opts.responseStyle === 'data' ? emptyData : { data: emptyData, ...result };
    }
    if (parseAs === 'stream') return opts.responseStyle === 'data' ? response.body : { data: response.body, ...result };
    let data: any;
    if (parseAs === 'json') {
      const text = await response.text();
      data = await validateAndTransform(text ? JSON.parse(text) : {}, opts);
    } else data = await response[parseAs]();
    return opts.responseStyle === 'data' ? data : { data, ...result };
  };

  const applyInterceptors = async <T>(value: T, fns: Array<((value: T, ...args: any[]) => T | Promise<T>) | null | undefined>, ...args: any[]): Promise<T> => {
    let current = value;
    for (const fn of fns) if (fn) current = await fn(current, ...args);
    return current;
  };

  const request: Client['request'] = async (options) => {`
    normalized = replaceGenerated(normalized, generated, compatible, relativePath)
    normalized = replaceGenerated(
      normalized,
      '      for (const fn of interceptors.request.fns) {\n        if (fn) {\n          request = await fn(request, opts);\n        }\n      }',
      '      request = await applyInterceptors(request, interceptors.request.fns, opts as ResolvedRequestOptions);',
      relativePath,
    )
    normalized = replaceGenerated(
      normalized,
      '      for (const fn of interceptors.response.fns) {\n        if (fn) {\n          response = await fn(response, request, opts);\n        }\n      }',
      '      response = await applyInterceptors(response, interceptors.response.fns, request, opts as ResolvedRequestOptions);',
      relativePath,
    )
    const okStart = normalized.indexOf('      if (response.ok) {')
    const okEnd = normalized.indexOf('\n\n      const textError', okStart)
    if (okStart >= 0 && okEnd >= 0) {
      normalized = `${normalized.slice(0, okStart)}      if (response.ok) return parseSuccessResponse(response, opts as ResolvedRequestOptions, request);${normalized.slice(okEnd)}`
    }
  }

  if (relativePath === 'client/utils.gen.ts') {
    normalized = replaceGenerated(
      normalized,
      'export const createQuerySerializer = <T = unknown>({\n  parameters = {},\n  ...args\n}: QuerySerializerOptions = {}): ((queryParams: T) => string) => {',
      "const serializeQueryParameter = (name: string, value: unknown, options: QuerySerializerOptions): string => {\n  if (Array.isArray(value)) {\n    return serializeArrayParam({ ...(options.allowReserved !== undefined ? { allowReserved: options.allowReserved } : {}), explode: true, name, style: 'form', value, ...options.array });\n  }\n  if (typeof value === 'object' && value !== null) {\n    return serializeObjectParam({ ...(options.allowReserved !== undefined ? { allowReserved: options.allowReserved } : {}), explode: true, name, style: 'deepObject', value: value as Record<string, unknown>, ...options.object });\n  }\n  return serializePrimitiveParam({ name, value: value as string, ...(options.allowReserved !== undefined ? { allowReserved: options.allowReserved } : {}) });\n};\n\nexport const createQuerySerializer = <T = unknown>({\n  parameters = {},\n  ...args\n}: QuerySerializerOptions = {}): ((queryParams: T) => string) => {",
      relativePath,
    )
    const loopStart = normalized.indexOf(
      "    if (queryParams && typeof queryParams === 'object') {",
    )
    const loopEnd = normalized.indexOf("    return search.join('&');", loopStart)
    if (loopStart >= 0 && loopEnd >= 0) {
      normalized = `${normalized.slice(0, loopStart)}    if (queryParams && typeof queryParams === 'object') {\n      for (const name in queryParams) {\n        const value = queryParams[name];\n        if (value == null) continue;\n        const serialized = serializeQueryParameter(name, value, parameters[name] || args);\n        if (serialized) search.push(serialized);\n      }\n    }\n${normalized.slice(loopEnd)}`
    }
    normalized = replaceGenerated(
      normalized,
      'export const mergeHeaders = (\n',
      "function appendHeader(merged: Headers, key: string, value: unknown): void {\n  if (value === null) { merged.delete(key); return; }\n  if (Array.isArray(value)) { value.forEach((item) => merged.append(key, item as string)); return; }\n  if (value !== undefined) merged.set(key, typeof value === 'object' ? JSON.stringify(value) : value as string);\n}\n\nexport const mergeHeaders = (\n",
      relativePath,
    )
    const mergeStart = normalized.indexOf('    for (const [key, value] of iterator) {')
    const mergeEnd = normalized.indexOf('    }\n  }\n  return mergedHeaders;', mergeStart)
    if (mergeStart >= 0 && mergeEnd >= 0) {
      normalized = `${normalized.slice(0, mergeStart)}    for (const [key, value] of iterator) appendHeader(mergedHeaders, key, value);\n${normalized.slice(mergeEnd + 6)}`
    }
  }

  return normalized
}

function normalizeCoreComplexity(relativePath, source) {
  let normalized = source
  if (relativePath === 'core/utils.gen.ts') {
    normalized = replaceGenerated(
      normalized,
      'export const defaultPathSerializer = ({ path, url: _url }: PathSerializer): string => {',
      "const serializePathValue = (name: string, value: unknown, explode: boolean, style: ArraySeparatorStyle): string | null => {\n  if (value == null) return null;\n  if (Array.isArray(value)) return serializeArrayParam({ explode, name, style, value });\n  if (typeof value === 'object') return serializeObjectParam({ explode, name, style: style === 'spaceDelimited' || style === 'pipeDelimited' ? 'form' : style, value: value as Record<string, unknown>, valueOnly: true });\n  if (style === 'matrix') return `;${serializePrimitiveParam({ name, value: value as string })}`;\n  return encodeURIComponent(style === 'label' ? `.${value as string}` : value as string);\n};\n\nexport const defaultPathSerializer = ({ path, url: _url }: PathSerializer): string => {",
      relativePath,
    )
    const valueStart = normalized.indexOf('      const value = path[name];')
    const valueEnd = normalized.indexOf('      url = url.replace(match, replaceValue);', valueStart)
    if (valueStart >= 0 && valueEnd >= 0) {
      normalized = `${normalized.slice(0, valueStart)}      const replaceValue = serializePathValue(name, path[name], explode, style);\n      if (replaceValue !== null) url = url.replace(match, replaceValue);${normalized.slice(valueEnd + '      url = url.replace(match, replaceValue);'.length)}`
    }
  }

  if (relativePath === 'core/params.gen.ts') {
    const generated =
      'export function buildClientParams(args: ReadonlyArray<unknown>, fields: FieldsConfig): Params {'
    const compatible = `function writeExtraParam(params: Params, config: FieldsConfig[number], key: string, value: unknown): void {
  const extra = extraPrefixes.find(([prefix]) => key.startsWith(prefix));
  if (extra) { ensureSlot(params, extra[1])[key.slice(extra[0].length)] = value; return; }
  if ('allowExtra' in config && config.allowExtra) {
    const slot = (Object.entries(config.allowExtra) as Array<[Slot, boolean]>).find(([, allowed]) => allowed)?.[0];
    if (slot) ensureSlot(params, slot)[key] = value;
  }
}

function ensureSlot(params: Params, slot: Slot): Record<string, unknown> {
  const record = params[slot] as Record<string, unknown> | undefined;
  if (record) return record;
  const created = Object.create(null) as Record<string, unknown>;
  params[slot] = created;
  return created;
}

function writeMappedParam(params: Params, map: KeyMap, config: FieldsConfig[number], arg: unknown): void {
  if ('in' in config) {
    if (!config.key) { params.body = arg; return; }
    const field = map.get(config.key);
    if (field?.in) ensureSlot(params, field.in)[field.map || config.key] = arg;
    return;
  }
  for (const [key, value] of Object.entries(arg ?? {})) {
    const field = map.get(key);
    if (field?.in) { ensureSlot(params, field.in)[field.map || key] = value; continue; }
    if (field) { params[field.map] = value; continue; }
    writeExtraParam(params, config, key, value);
  }
}

export function buildClientParams(args: ReadonlyArray<unknown>, fields: FieldsConfig): Params {`
    normalized = replaceGenerated(normalized, generated, compatible, relativePath)
    const bodyStart = normalized.indexOf(
      "    if ('in' in config) {",
      normalized.indexOf('export function buildClientParams'),
    )
    const bodyEnd = normalized.indexOf('    }\n  }\n\n  stripEmptySlots', bodyStart)
    if (bodyStart >= 0 && bodyEnd >= 0) {
      normalized = `${normalized.slice(0, bodyStart)}    writeMappedParam(params, map, config, arg);\n${normalized.slice(bodyEnd + 6)}`
    }
  }

  return normalized
}

function normalizeSseComplexity(relativePath, source) {
  let normalized = source
  if (relativePath === 'core/serverSentEvents.gen.ts') {
    const start = normalized.indexOf('export function createSseClient')
    const endMarker = '\n  return { stream };\n}'
    const end = normalized.indexOf(endMarker, start)
    if (start >= 0 && end >= 0) {
      const replacement = `type SseState = Readonly<{ lastEventId?: string; retryDelay: number }>;

type ParsedSseChunk<TData> = Readonly<{
  event: StreamEvent<TData>;
  hasData: boolean;
  isJson: boolean;
  state: SseState;
}>;

function parseSseField(state: Readonly<{ dataLines: string[]; eventName?: string; lastEventId?: string; retryDelay: number }>, line: string) {
  if (line.startsWith('data:')) return { ...state, dataLines: [...state.dataLines, line.replace(/^data:\\s*/, '')] };
  if (line.startsWith('event:')) return { ...state, eventName: line.replace(/^event:\\s*/, '') };
  if (line.startsWith('id:')) return { ...state, lastEventId: line.replace(/^id:\\s*/, '') };
  if (!line.startsWith('retry:')) return state;
  const parsed = Number.parseInt(line.replace(/^retry:\\s*/, ''), 10);
  return Number.isNaN(parsed) ? state : { ...state, retryDelay: parsed };
}

function parseSseData(dataLines: string[]): Readonly<{ data: unknown; isJson: boolean }> {
  if (!dataLines.length) return { data: undefined, isJson: false };
  const rawData = dataLines.join('\\n');
  try { return { data: JSON.parse(rawData), isJson: true }; }
  catch { return { data: rawData, isJson: false }; }
}

function parseSseChunk<TData>(chunk: string, previous: SseState): ParsedSseChunk<TData> {
  const initial = { dataLines: [], retryDelay: previous.retryDelay, ...(previous.lastEventId !== undefined ? { lastEventId: previous.lastEventId } : {}) };
  const parsed = chunk.split('\\n').reduce(
    (state, line) => parseSseField(state, line),
    initial,
  );
  const { data, isJson } = parseSseData(parsed.dataLines);
  const state = { retryDelay: parsed.retryDelay, ...(parsed.lastEventId !== undefined ? { lastEventId: parsed.lastEventId } : {}) };
  const event = { data: data as TData, retry: state.retryDelay, ...(parsed.eventName !== undefined ? { event: parsed.eventName } : {}), ...(state.lastEventId !== undefined ? { id: state.lastEventId } : {}) };
  return { event, hasData: parsed.dataLines.length > 0, isJson, state };
}

async function prepareSseChunk<TData>(parsed: ParsedSseChunk<TData>, validator?: (data: unknown) => Promise<unknown>, transformer?: (data: unknown) => Promise<unknown>): Promise<ParsedSseChunk<TData>> {
  if (!parsed.isJson) return parsed;
  if (validator) await validator(parsed.event.data);
  const data = transformer ? await transformer(parsed.event.data) as TData : parsed.event.data;
  return { ...parsed, event: { ...parsed.event, data } };
}

async function* readSseResponse<TData>(response: Response, signal: AbortSignal, initialState: SseState, onState: (state: SseState) => void, onEvent?: (event: StreamEvent<TData>) => void, validator?: (data: unknown) => Promise<unknown>, transformer?: (data: unknown) => Promise<unknown>): AsyncGenerator<TData> {
  if (!response.body) throw new Error('No body in SSE response');
  const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
  const abortHandler = () => { reader.cancel().catch(() => {}); };
  signal.addEventListener('abort', abortHandler);
  let buffer = '';
  let state = initialState;
  try {
    while (true) {
      const read = await reader.read();
      if (read.done) break;
      buffer = (buffer + read.value).replace(/\\r\\n?/g, '\\n');
      const chunks = buffer.split('\\n\\n');
      buffer = chunks.pop() ?? '';
      for (const chunk of chunks) {
        const parsed = await prepareSseChunk(parseSseChunk<TData>(chunk, state), validator, transformer);
        state = parsed.state;
        onState(state);
        onEvent?.(parsed.event);
        if (parsed.hasData) yield parsed.event.data;
      }
    }
  } finally {
    signal.removeEventListener('abort', abortHandler);
    reader.releaseLock();
  }
}

async function fetchSseResponse(url: string, options: ServerSentEventsOptions, signal: AbortSignal, state: SseState, onRequest: ServerSentEventsOptions['onRequest']): Promise<Response> {
  const headers = new Headers(options.headers as HeadersInit | undefined);
  if (state.lastEventId !== undefined) headers.set('Last-Event-ID', state.lastEventId);
  const init: RequestInit = { redirect: 'follow', ...options, headers, signal, ...(options.serializedBody !== undefined ? { body: options.serializedBody } : {}) };
  const request = onRequest ? await onRequest(url, init) : new Request(url, init);
  const response = await (options.fetch ?? globalThis.fetch)(request);
  if (!response.ok) throw new Error(\`SSE failed: \${response.status} \${response.statusText}\`);
  return response;
}

export function createSseClient<TData = unknown>({ onRequest, onSseError, onSseEvent, responseTransformer, responseValidator, sseDefaultRetryDelay, sseMaxRetryAttempts, sseMaxRetryDelay, sseSleepFn, url, ...options }: ServerSentEventsOptions): ServerSentEventsResult<TData> {
  const sleep = sseSleepFn ?? ((ms: number) => new Promise((resolve) => setTimeout(resolve, ms)));
  const createStream = async function* () {
    let state: SseState = { retryDelay: sseDefaultRetryDelay ?? 3000 };
    let attempt = 0;
    const signal = options.signal ?? new AbortController().signal;
    while (!signal.aborted) {
      attempt += 1;
      try {
        const response = await fetchSseResponse(url, { ...options, url }, signal, state, onRequest);
        const updateState = (next: SseState) => { state = next; };
        yield* readSseResponse<TData>(response, signal, state, updateState, onSseEvent, responseValidator, responseTransformer);
        return;
      } catch (error) {
        onSseError?.(error);
        if (sseMaxRetryAttempts !== undefined && attempt >= sseMaxRetryAttempts) break;
        await sleep(Math.min(state.retryDelay * 2 ** (attempt - 1), sseMaxRetryDelay ?? 30000));
      }
    }
  };
  return { stream: createStream() as ServerSentEventsResult<TData>['stream'] };
}`
      normalized = `${normalized.slice(0, start)}${replacement}${normalized.slice(end + endMarker.length)}`
    }
  }

  return normalized
}

function normalizeReactQueryComplexity(relativePath, source) {
  let normalized = source
  if (relativePath === '@tanstack/react-query.gen.ts') {
    if (
      normalized.includes(
        'baseUrl: options?.baseUrl || (options?.client ?? client).getConfig().baseUrl',
      ) ||
      normalized.includes(
        'baseUrl: options?.baseUrl ?? (options?.client ?? client).getConfig().baseUrl',
      )
    ) {
      normalized = replaceGenerated(
        normalized,
        'baseUrl: options?.baseUrl || (options?.client ?? client).getConfig().baseUrl',
        'baseUrl: options?.baseUrl ?? (options?.client ?? client).getConfig().baseUrl',
        relativePath,
      )
    }
    const legacyInfiniteParams =
      "const createInfiniteParams = <K extends Pick<QueryKey<Options>[0], 'body' | 'headers' | 'path' | 'query'>>(queryKey: QueryKey<Options>, page: K) => {"
    const compatibleInfiniteParams =
      'const createInfiniteParams = <K extends InfinitePageParams>(queryKey: QueryKey<Options>, page: K) => {'
    const infinitePageParams =
      "type InfinitePageParams = Pick<QueryKey<Options>[0], 'body' | 'headers' | 'path' | 'query'>;"
    const genericInfinitePageParams =
      "type GenericInfinitePageParams<TData> = Pick<QueryKey<Options<TData>>[0], 'body' | 'headers' | 'path' | 'query'>;"
    const infinitePageParam = 'type InfinitePageParam = number | InfinitePageParams;'
    const infiniteStringPageParam = 'type InfiniteStringPageParam = string | InfinitePageParams;'
    if (normalized.includes(legacyInfiniteParams)) {
      normalized = replaceGenerated(
        normalized,
        legacyInfiniteParams,
        `${infinitePageParams}\n\n${infinitePageParam}\n\n${compatibleInfiniteParams}`,
        relativePath,
      )
    } else if (
      normalized.includes(compatibleInfiniteParams) &&
      !normalized.includes(infinitePageParam)
    ) {
      normalized = replaceGenerated(
        normalized,
        `${infinitePageParams}\n\n${compatibleInfiniteParams}`,
        `${infinitePageParams}\n\n${infinitePageParam}\n\n${compatibleInfiniteParams}`,
        relativePath,
      )
    }
    if (normalized.includes(infinitePageParam)) {
      normalized = normalized.replaceAll(
        /number \| Pick<QueryKey<Options<[^>]+>>\[0\], 'body' \| 'headers' \| 'path' \| 'query'>/g,
        'InfinitePageParam',
      )
      normalized = normalized.replaceAll(
        /string \| Pick<QueryKey<Options<[^>]+>>\[0\], 'body' \| 'headers' \| 'path' \| 'query'>/g,
        'InfiniteStringPageParam',
      )
      normalized = normalized.replaceAll(
        /Pick<QueryKey<Options<([^>]+)>>\[0\], 'body' \| 'headers' \| 'path' \| 'query'>/g,
        'GenericInfinitePageParams<$1>',
      )
      if (!normalized.includes(genericInfinitePageParams)) {
        normalized = normalized.replace(
          infinitePageParam,
          `${infinitePageParam}\n\n${infiniteStringPageParam}\n\n${genericInfinitePageParams}`,
        )
      }
    }
  }

  return normalized
}

function normalizeGeneratedTypes(relativePath, source) {
  if (relativePath !== 'core/types.gen.ts') {
    return source
  }

  let normalized = replaceGenerated(
    source,
    '        string | number | boolean | (string | number | boolean)[] | null | undefined | unknown\n',
    '        unknown\n',
    relativePath,
  )
  normalized = replaceGenerated(
    normalized,
    '// eslint-disable-next-line @typescript-eslint/no-empty-object-type\nexport interface ClientMeta {}',
    'export type ClientMeta = Record<never, never>',
    relativePath,
  )
  return replaceGenerated(
    normalized,
    'type IsExactlyNeverOrNeverUndefined<T> = [T] extends [never]\n  ? true\n  : [T] extends [never | undefined]\n    ? [undefined] extends [T]\n      ? false\n      : true\n    : false;',
    'type IsNever<T> = [T] extends [never] ? true : false;',
    relativePath,
  ).replaceAll('IsExactlyNeverOrNeverUndefined<T[K]>', 'IsNever<T[K]>')
}

function trimGeneratedTrailingWhitespace(source) {
  return source
    .split('\n')
    .map((line) => line.trimEnd())
    .join('\n')
}

async function normalizeDirectory(directory) {
  const entries = await readdir(directory, { withFileTypes: true })
  await Promise.all(
    entries.map(async (entry) => {
      const entryPath = path.join(directory, entry.name)
      if (entry.isDirectory()) {
        await normalizeDirectory(entryPath)
        return
      }
      if (!entry.isFile() || !entry.name.endsWith('.ts')) {
        return
      }

      const source = await readFile(entryPath, 'utf8')
      const relativePath = path.relative(outputPath, entryPath).split(path.sep).join('/')
      const normalized = trimGeneratedTrailingWhitespace(
        normalizeGeneratedTypes(
          relativePath,
          normalizeReactQueryComplexity(
            relativePath,
            normalizeSseComplexity(
              relativePath,
              normalizeCoreComplexity(
                relativePath,
                normalizeClientComplexity(
                  relativePath,
                  normalizeGeneratedFindings(
                    relativePath,
                    normalizeExactOptionalProperties(relativePath, source),
                  ),
                ),
              ),
            ),
          ),
        ),
      )
      if (normalized !== source) {
        await writeFile(entryPath, normalized, 'utf8')
      }
    }),
  )
}

await normalizeDirectory(outputPath)
