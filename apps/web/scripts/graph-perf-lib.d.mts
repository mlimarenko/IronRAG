export type CpuProfileCallFrame = Readonly<{
  functionName: string
  lineNumber: number
  url: string
}>

export type CpuProfile = Readonly<{
  nodes: ReadonlyArray<
    Readonly<{
      callFrame: CpuProfileCallFrame
      id: number
    }>
  >
  samples?: ReadonlyArray<number>
  timeDeltas?: ReadonlyArray<number>
}>

export function getJankTag(longTaskMax: number): string
export function parseSessionCookie(setCookie: string): [string, string] | null
export function summarizeCpuProfile(profile: CpuProfile, limit?: number): Array<[string, number]>
export function buildBrowserLaunchOptions(
  browserName: string,
  headless: boolean,
  gpuArgs: string,
): Readonly<{ headless: boolean; args?: string[] }>
