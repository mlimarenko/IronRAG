import { chromium, type FullConfig } from '@playwright/test'
import { randomUUID } from 'node:crypto'
import { lstat, mkdir, open, rename, unlink } from 'node:fs/promises'
import { isIP } from 'node:net'
import { basename, dirname, isAbsolute, join, parse, relative, resolve, sep } from 'node:path'

function isMissingFile(error: unknown) {
  return error instanceof Error && 'code' in error && error.code === 'ENOENT'
}

function isExistingFile(error: unknown) {
  return error instanceof Error && 'code' in error && error.code === 'EEXIST'
}

function isLoopbackHostname(hostname: string): boolean {
  if (hostname === 'localhost') return true

  const unbracketedHostname =
    hostname.startsWith('[') && hostname.endsWith(']') ? hostname.slice(1, -1) : hostname
  if (isIP(unbracketedHostname) === 6) return unbracketedHostname === '::1'
  if (isIP(unbracketedHostname) !== 4) return false

  return Number.parseInt(unbracketedHostname.split('.')[0] ?? '', 10) === 127
}

export function assertSafeQaLoginUrl(baseURL: string): URL {
  const parsed = new URL(baseURL)
  if (parsed.protocol === 'https:') return parsed
  if (parsed.protocol !== 'http:' || !isLoopbackHostname(parsed.hostname)) {
    throw new Error('Visual QA login requires HTTPS unless QA_BASE_URL uses loopback')
  }
  return parsed
}

function assertPathHasNoTraversal(storageStatePath: string) {
  if (!storageStatePath || storageStatePath.includes('\0')) {
    throw new Error('QA_STORAGE_STATE_PATH must name a JSON file')
  }
  if (storageStatePath.split(/[\\/]/u).includes('..')) {
    throw new Error('QA_STORAGE_STATE_PATH must not contain parent-directory traversal')
  }
}

function assertDirectoryEntry(path: string, entry: Awaited<ReturnType<typeof lstat>>) {
  if (entry.isSymbolicLink()) {
    throw new Error(`QA storage path must not traverse a symbolic link: ${path}`)
  }
  if (!entry.isDirectory()) {
    throw new Error(`QA storage parent is not a directory: ${path}`)
  }
}

async function ensureStorageDirectory(directoryPath: string) {
  const { root } = parse(directoryPath)
  const pathSegments = relative(root, directoryPath).split(sep).filter(Boolean)
  let currentPath = root

  for (const pathSegment of pathSegments) {
    currentPath = join(currentPath, pathSegment)
    try {
      assertDirectoryEntry(currentPath, await lstat(currentPath))
      continue
    } catch (error) {
      if (!isMissingFile(error)) throw error
    }

    let created = false
    try {
      await mkdir(currentPath, { mode: 0o700 })
      created = true
    } catch (error) {
      if (!isExistingFile(error)) throw error
    }

    const entry = await lstat(currentPath)
    assertDirectoryEntry(currentPath, entry)
    if (created && (entry.mode & 0o077) !== 0) {
      throw new Error(`QA storage directory was not created privately: ${currentPath}`)
    }
  }
}

async function assertSafeStorageTarget(storageStatePath: string) {
  try {
    const entry = await lstat(storageStatePath)
    if (entry.isSymbolicLink()) {
      throw new Error('QA_STORAGE_STATE_PATH must not be a symbolic link')
    }
    if (!entry.isFile()) {
      throw new Error('QA_STORAGE_STATE_PATH must name a regular file')
    }
  } catch (error) {
    if (!isMissingFile(error)) throw error
  }
}

export async function writeQaStorageState(
  requestedPath: string,
  storageState: unknown,
  currentWorkingDirectory = process.cwd(),
): Promise<string> {
  assertPathHasNoTraversal(requestedPath)
  const storageStatePath = isAbsolute(requestedPath)
    ? resolve(requestedPath)
    : resolve(currentWorkingDirectory, requestedPath)
  const storageStateDirectory = dirname(storageStatePath)

  await ensureStorageDirectory(storageStateDirectory)
  await assertSafeStorageTarget(storageStatePath)

  const serializedStorageState = JSON.stringify(storageState)
  if (serializedStorageState === undefined) {
    throw new Error('Visual QA storage state must be JSON serializable')
  }

  const temporaryStatePath = join(
    storageStateDirectory,
    `.${basename(storageStatePath)}.${process.pid}.${randomUUID()}.tmp`,
  )
  let temporaryFile: Awaited<ReturnType<typeof open>> | undefined
  try {
    temporaryFile = await open(temporaryStatePath, 'wx', 0o600)
    await temporaryFile.chmod(0o600)
    await temporaryFile.writeFile(`${serializedStorageState}\n`, 'utf8')
    await temporaryFile.sync()
    await temporaryFile.close()
    temporaryFile = undefined

    // Revalidate immediately before publication. rename(2) replaces a directory
    // entry instead of following it, while the published file inherits the
    // already-private mode from the temporary file.
    await assertSafeStorageTarget(storageStatePath)
    await rename(temporaryStatePath, storageStatePath)

    const publishedState = await lstat(storageStatePath)
    if (!publishedState.isFile() || publishedState.isSymbolicLink()) {
      throw new Error('Visual QA storage state was not published as a regular file')
    }
    if ((publishedState.mode & 0o777) !== 0o600) {
      throw new Error('Visual QA storage state was not published with mode 0600')
    }
    return storageStatePath
  } catch (error) {
    if (temporaryFile) await temporaryFile.close()
    try {
      await unlink(temporaryStatePath)
    } catch (cleanupError) {
      if (!isMissingFile(cleanupError)) throw cleanupError
    }
    throw error
  }
}

/**
 * Log in to the real backend once before the visual QA run, and persist
 * the session cookies so every scenario renders against the live API. We
 * talk through the vite dev server (which proxies `/v1` to the backend
 * at 127.0.0.1:19000) so the cookie domain matches the test's baseURL.
 *
 * Credentials are injected via `QA_LOGIN` / `QA_PASSWORD` env vars.
 */
async function globalSetup(_config: FullConfig) {
  const login = process.env.QA_LOGIN ?? 'admin'
  const password = process.env.QA_PASSWORD
  const baseURL = process.env.QA_BASE_URL ?? 'http://127.0.0.1:4173'
  assertSafeQaLoginUrl(baseURL)
  const storageStatePath = process.env.QA_STORAGE_STATE_PATH ?? 'visual-qa/.storage/state.json'
  if (!password) {
    throw new Error('QA_PASSWORD is required for visual QA login')
  }

  const browser = await chromium.launch()
  try {
    const context = await browser.newContext({ baseURL })
    const response = await context.request.post(`${baseURL}/v1/iam/sessions`, {
      data: { login, password },
      headers: { 'content-type': 'application/json' },
    })
    if (!response.ok()) {
      throw new Error(`Login failed: HTTP ${response.status()}`)
    }
    // Fetch /iam/session/resolve so we have a fresh session record and the
    // backend sees one full round-trip before the scenarios run.
    const resolve = await context.request.get(`${baseURL}/v1/iam/context`)
    if (!resolve.ok()) {
      throw new Error(`Session resolve failed: HTTP ${resolve.status()}`)
    }
    const storageState = await context.storageState()
    await writeQaStorageState(storageStatePath, storageState)
    await context.close()
  } finally {
    await browser.close()
  }
}

export default globalSetup
