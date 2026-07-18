import { System } from './generated'
import type { ReleaseUpdateResponse } from './generated'
import { unwrap } from './runtime'

export const versionApi = {
  getReleaseUpdate: () =>
    System.getReleaseUpdate().then((result): ReleaseUpdateResponse => unwrap(result)),
}
