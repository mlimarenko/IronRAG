/**
 * Which of the four mutually exclusive states a query-backed screen is in.
 *
 * Every list and detail screen answers the same question before it renders
 * anything — "am I still waiting, did it fail, is there nothing to show, or do
 * I have content?" — and each one used to answer it with its own chain of
 * nested ternaries inline in JSX. The chains were the same decision written
 * twenty times, so they drifted: some treated a failed refresh of already
 * rendered data as a screen failure, some ranked "empty" above "error".
 *
 * The decision lives here as a pure function so it can be read and tested
 * without mounting a component, and so the ranking is stated once.
 */
export type ScreenPhase = 'loading' | 'error' | 'empty' | 'ready'

export type ScreenPhaseInput = Readonly<{
  /** The first load has not settled yet. Outranks everything. */
  loading: boolean
  /** The load failed *and* there is nothing already on screen to keep. */
  failed: boolean
  /** The load succeeded and returned no rows. */
  empty?: boolean
}>

/**
 * Ranking, highest first: `loading` -> `error` -> `empty` -> `ready`.
 *
 * `failed` is deliberately not "the query errored": a poll that fails while
 * content is already rendered is a background event, and replacing the screen
 * with an error page throws away what the operator was reading. Callers pass
 * `query.isError && !data` when they want that behaviour.
 */
export function screenPhase(input: ScreenPhaseInput): ScreenPhase {
  if (input.loading) return 'loading'
  if (input.failed) return 'error'
  if (input.empty === true) return 'empty'
  return 'ready'
}
