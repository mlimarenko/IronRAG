import { apiFetch } from "./client";

// Types matching the backend API responses
export interface BootstrapProviderPreset {
  bindingPurpose: string;
  modelCatalogId: string;
  modelName: string;
  presetName: string;
  systemPrompt?: string | null;
  temperature?: number | null;
  topP?: number | null;
  maxOutputTokensOverride?: number | null;
}

export interface BootstrapProviderPresetBundle {
  id: string;
  providerKind: string;
  displayName: string;
  credentialSource: string;
  defaultBaseUrl?: string | null;
  apiKeyRequired: boolean;
  baseUrlRequired: boolean;
  presets: BootstrapProviderPreset[];
}

export interface BootstrapStatus {
  setupRequired: boolean;
  aiSetup: {
    presetBundles: BootstrapProviderPresetBundle[];
  } | null;
}

export interface SessionResolveResponse {
  mode: "authenticated" | "guest" | "bootstrap";
  locale: string;
  session: { id: string; expiresAt: string } | null;
  me: { principal: { id: string; displayLabel: string }; user: { login: string; displayName: string } | null } | null;
  shellBootstrap: {
    workspaces: Array<{ id: string; slug: string; name: string }>;
    libraries: Array<{ id: string; workspaceId: string; slug: string; name: string; ingestionReady: boolean; missingBindingPurposes: string[] }>;
  } | null;
  bootstrapStatus: { setupRequired: boolean } | null;
  message: string | null;
}

export interface LoginResponse {
  sessionId: string;
  expiresAt: string;
  user: { principalId: string; login: string; displayName: string };
}

export const authApi = {
  getBootstrapStatus: () => apiFetch<BootstrapStatus>("/iam/bootstrap/status"),
  resolveSession: () => apiFetch<SessionResolveResponse>("/iam/session/resolve"),
  login: (login: string, password: string) => apiFetch<LoginResponse>("/iam/session/login", {
    method: "POST",
    body: JSON.stringify({ login, password }),
  }),
  logout: () => apiFetch<void>("/iam/session/logout", { method: "POST" }),
  bootstrapSetup: (data: { login: string; password: string; displayName: string; aiSetup?: { providerKind: string; apiKey?: string; baseUrl?: string } }) =>
    apiFetch<LoginResponse>("/iam/bootstrap/setup", {
      method: "POST",
      body: JSON.stringify(data),
    }),
};
