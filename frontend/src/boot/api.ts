import axios from 'axios'

interface FrontendEnv {
  readonly VITE_BACKEND_URL?: string
}

const env = import.meta.env as ImportMetaEnv & FrontendEnv
const backendUrl: string = env.VITE_BACKEND_URL ?? 'http://127.0.0.1:8080'

export const api = axios.create({
  baseURL: backendUrl,
})
