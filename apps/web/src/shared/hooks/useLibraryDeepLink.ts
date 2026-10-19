import { useEffect } from 'react'
import type { SetURLSearchParams } from 'react-router-dom'

/**
 * Applies a `?lib=<id>` deep link to the shell's active library, once.
 *
 * Extracted from the AI configuration screen so every screen that can be linked
 * to behaves the same. It was only implemented there, so the same link on the
 * assistant silently kept whatever library happened to be active — the reader
 * saw a page titled for one library while it answered from another.
 *
 * The two rules the original comments earned are preserved exactly:
 *
 * - The link stays in the address until the shell has actually landed on it.
 *   Erasing it the moment resolution merely STARTS drops the instruction
 *   whenever the lookup fails, leaving the screen on the previously active
 *   library with nothing to say the link was lost.
 * - Once landed, the parameter is removed. A link says "go here once"; left in
 *   the address it re-applies on every later change and makes the on-screen
 *   library switcher inert.
 */
export function useLibraryDeepLink({
  params,
  setParams,
  activeLibraryId,
  selectLibrary,
}: {
  params: URLSearchParams
  setParams: SetURLSearchParams
  activeLibraryId: string | undefined
  selectLibrary: (libraryId: string) => void
}) {
  useEffect(() => {
    const libraryId = params.get('lib')
    if (!libraryId) return
    if (libraryId !== activeLibraryId) {
      selectLibrary(libraryId)
      return
    }
    const next = new URLSearchParams(params)
    next.delete('lib')
    setParams(next, { replace: true })
  }, [activeLibraryId, params, selectLibrary, setParams])
}
