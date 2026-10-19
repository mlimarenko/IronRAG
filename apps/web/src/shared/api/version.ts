import { System } from './generated'
import type { ReleaseUpdateResponse } from './generated'
import { unwrap } from './runtime'

export const versionApi = {
  getLatestVersion: () =>
    System.getLatestVersion().then((result): ReleaseUpdateResponse => unwrap(result)),
}
