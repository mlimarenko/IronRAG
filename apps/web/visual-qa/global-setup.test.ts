import { afterEach, describe, expect, it } from 'vitest'
import { chmod, lstat, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { assertSafeQaLoginUrl, writeQaStorageState } from './global-setup'

const temporaryDirectories: string[] = []

async function makeTemporaryDirectory() {
  const directory = await mkdtemp(join(tmpdir(), 'ironrag-visual-qa-'))
  temporaryDirectories.push(directory)
  return directory
}

function permissions(mode: number) {
  return mode & 0o777
}

afterEach(async () => {
  await Promise.all(
    temporaryDirectories
      .splice(0)
      .map((directory) => rm(directory, { recursive: true, force: true })),
  )
})

describe('external visual QA login URL', () => {
  it('requires HTTPS for every non-loopback host', () => {
    expect(() => assertSafeQaLoginUrl('http://example.test')).toThrow(
      'Visual QA login requires HTTPS',
    )
    expect(assertSafeQaLoginUrl('https://example.test').protocol).toBe('https:')
  })

  it.each([
    'http://localhost:4173',
    'http://127.0.0.1:4173',
    'http://127.20.30.40:4173',
    'http://2130706433:4173',
    'http://0x7f000001:4173',
    'http://[::1]:4173',
    'http://[0:0:0:0:0:0:0:1]:4173',
  ])('allows the loopback development URL %s', (url) => {
    expect(assertSafeQaLoginUrl(url).hostname).toBeTruthy()
  })

  it.each([
    'http://127.attacker.example:4173',
    'http://127.0.0.1.attacker.example:4173',
    'http://localhost.attacker.example:4173',
    'http://app.localhost:4173',
    'http://[::ffff:127.0.0.1]:4173',
    'ftp://localhost:4173',
  ])('rejects the non-loopback plaintext URL %s', (url) => {
    expect(() => assertSafeQaLoginUrl(url)).toThrow('Visual QA login requires HTTPS')
  })
})

describe('visual QA storage state', () => {
  const storageState = { cookies: [], origins: [] }

  it('does not change an existing parent mode and atomically publishes a 0600 file', async () => {
    const directory = await makeTemporaryDirectory()
    await chmod(directory, 0o755)
    const storageStatePath = join(directory, 'state.json')
    await writeFile(storageStatePath, 'stale\n', { mode: 0o644 })

    await expect(writeQaStorageState(storageStatePath, storageState)).resolves.toBe(
      storageStatePath,
    )

    expect(permissions((await lstat(directory)).mode)).toBe(0o755)
    expect(permissions((await lstat(storageStatePath)).mode)).toBe(0o600)
    expect(await readFile(storageStatePath, 'utf8')).toBe(`${JSON.stringify(storageState)}\n`)
  })

  it('creates a missing task directory privately', async () => {
    const rootDirectory = await makeTemporaryDirectory()
    const privateDirectory = join(rootDirectory, 'private-state')
    const storageStatePath = join(privateDirectory, 'state.json')

    await writeQaStorageState(storageStatePath, storageState)

    expect(permissions((await lstat(privateDirectory)).mode)).toBe(0o700)
    expect(permissions((await lstat(storageStatePath)).mode)).toBe(0o600)
  })

  it('does not chmod the working directory for a relative basename', async () => {
    const workingDirectory = await makeTemporaryDirectory()
    await chmod(workingDirectory, 0o750)

    await writeQaStorageState('state.json', storageState, workingDirectory)

    expect(permissions((await lstat(workingDirectory)).mode)).toBe(0o750)
    expect(permissions((await lstat(join(workingDirectory, 'state.json'))).mode)).toBe(0o600)
  })

  it('does not change the mode of an existing absolute parent such as /tmp', async () => {
    const temporaryRootMode = permissions((await lstat(tmpdir())).mode)
    const storageStatePath = join(tmpdir(), `ironrag-visual-qa-state-${process.pid}.json`)

    try {
      await writeQaStorageState(storageStatePath, storageState)

      expect(permissions((await lstat(tmpdir())).mode)).toBe(temporaryRootMode)
      expect(permissions((await lstat(storageStatePath)).mode)).toBe(0o600)
    } finally {
      await rm(storageStatePath, { force: true })
    }
  })

  it('rejects a symbolic-link target without modifying its destination', async () => {
    const directory = await makeTemporaryDirectory()
    const destinationPath = join(directory, 'destination.json')
    const storageStatePath = join(directory, 'state.json')
    await writeFile(destinationPath, 'sentinel\n', { mode: 0o600 })
    await symlink(destinationPath, storageStatePath)

    await expect(writeQaStorageState(storageStatePath, storageState)).rejects.toThrow(
      'QA_STORAGE_STATE_PATH must not be a symbolic link',
    )
    expect(await readFile(destinationPath, 'utf8')).toBe('sentinel\n')
  })

  it('rejects a symbolic-link parent', async () => {
    const directory = await makeTemporaryDirectory()
    const destinationDirectory = join(directory, 'destination')
    const linkedDirectory = join(directory, 'linked')
    await mkdir(destinationDirectory, { mode: 0o700 })
    await symlink(destinationDirectory, linkedDirectory)

    await expect(
      writeQaStorageState(join(linkedDirectory, 'state.json'), storageState),
    ).rejects.toThrow('QA storage path must not traverse a symbolic link')
  })

  it('rejects parent-directory traversal in a custom relative path', async () => {
    const workingDirectory = await makeTemporaryDirectory()

    await expect(
      writeQaStorageState('../state.json', storageState, workingDirectory),
    ).rejects.toThrow('QA_STORAGE_STATE_PATH must not contain parent-directory traversal')
  })
})
