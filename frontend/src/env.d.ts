/// <reference types="vite/client" />

declare module 'quasar/wrappers' {
  export function configure<T>(fn: () => T): T
}
