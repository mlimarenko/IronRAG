import type {
  AIProvider,
  AIProviderBaseUrlPolicy,
  AIProviderCredentialPolicy,
  AIProviderModelDiscovery,
} from "@/shared/types";

type BootstrapProviderPresetBundleInput = {
  providerKind: string;
  credentialSource: string;
  defaultBaseUrl?: string | null;
  baseUrlPolicy: unknown;
};

type BootstrapAiSetupInput = {
  providerKind: string;
  apiKey?: string;
  baseUrl?: string;
};

export function hasStoredApiKeySummary(apiKeySummary?: string | null): boolean {
  return Boolean(apiKeySummary && apiKeySummary !== "not_configured");
}

type CredentialPolicyInput = {
  credentialPolicy: unknown;
};

type BaseUrlPolicyInput = {
  credentialSource?: string;
  defaultBaseUrl?: string | null;
  baseUrlPolicy: unknown;
};

type ModelDiscoveryInput = {
  modelDiscovery: unknown;
};

const baseUrlModes = new Set(["fixed", "required", "optional"]);
const credentialValidationModes = new Set(["chat_round_trip", "model_list", "none"]);
const modelDiscoveryModes = new Set(["shared", "credential", "unsupported"]);

function assertRecord(value: unknown, policyName: string): asserts value is Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`Invalid AI provider metadata: ${policyName} must be an object`);
  }
}

function assertBooleanField(
  value: Record<string, unknown>,
  fieldName: string,
  policyName: string,
): asserts value is Record<string, unknown> & Record<typeof fieldName, boolean> {
  if (typeof value[fieldName] !== "boolean") {
    throw new Error(`Invalid AI provider metadata: ${policyName}.${fieldName} must be boolean`);
  }
}

function assertStringField(
  value: Record<string, unknown>,
  fieldName: string,
  policyName: string,
): asserts value is Record<string, unknown> & Record<typeof fieldName, string> {
  if (typeof value[fieldName] !== "string" || value[fieldName].trim() === "") {
    throw new Error(`Invalid AI provider metadata: ${policyName}.${fieldName} must be string`);
  }
}

function assertStringEnumField(
  value: Record<string, unknown>,
  fieldName: string,
  policyName: string,
  allowedValues: Set<string>,
): asserts value is Record<string, unknown> & Record<typeof fieldName, string> {
  if (typeof value[fieldName] !== "string" || !allowedValues.has(value[fieldName])) {
    throw new Error(`Invalid AI provider metadata: ${policyName}.${fieldName} is not canonical`);
  }
}

function assertCredentialPolicy(value: unknown): asserts value is AIProviderCredentialPolicy {
  assertRecord(value, "credentialPolicy");
  assertBooleanField(value, "apiKeyRequired", "credentialPolicy");
  assertBooleanField(value, "baseUrlRequired", "credentialPolicy");
  assertStringEnumField(value, "baseUrlMode", "credentialPolicy", baseUrlModes);
  assertStringEnumField(value, "validationMode", "credentialPolicy", credentialValidationModes);
}

function assertBaseUrlPolicy(value: unknown): asserts value is AIProviderBaseUrlPolicy {
  assertRecord(value, "baseUrlPolicy");
  assertBooleanField(value, "allowOverride", "baseUrlPolicy");
  assertBooleanField(value, "requireHttps", "baseUrlPolicy");
  assertBooleanField(value, "allowPrivateNetwork", "baseUrlPolicy");
  if (!Array.isArray(value.trimSuffixes) || value.trimSuffixes.some((entry) => typeof entry !== "string")) {
    throw new Error("Invalid AI provider metadata: baseUrlPolicy.trimSuffixes must be string[]");
  }
}

function assertModelDiscovery(value: unknown): asserts value is AIProviderModelDiscovery {
  assertRecord(value, "modelDiscovery");
  assertStringEnumField(value, "mode", "modelDiscovery", modelDiscoveryModes);
  if (!Array.isArray(value.paths)) {
    throw new Error("Invalid AI provider metadata: modelDiscovery.paths must be an array");
  }
  for (const path of value.paths) {
    assertRecord(path, "modelDiscovery.paths[]");
    assertStringField(path, "capabilityKind", "modelDiscovery.paths[]");
    assertStringField(path, "path", "modelDiscovery.paths[]");
  }
}

export function resolveProviderCredentialPolicy(
  provider: CredentialPolicyInput,
): AIProviderCredentialPolicy {
  assertCredentialPolicy(provider.credentialPolicy);
  return provider.credentialPolicy;
}

export function resolveProviderBaseUrlPolicy(
  provider: Pick<BaseUrlPolicyInput, "baseUrlPolicy">,
): AIProviderBaseUrlPolicy {
  assertBaseUrlPolicy(provider.baseUrlPolicy);
  return provider.baseUrlPolicy;
}

export function resolveProviderModelDiscovery(
  provider: ModelDiscoveryInput,
): AIProviderModelDiscovery {
  assertModelDiscovery(provider.modelDiscovery);
  return provider.modelDiscovery;
}

export function normalizeProviderBaseUrl(
  provider: Pick<BaseUrlPolicyInput, "baseUrlPolicy">,
  baseUrl?: string | null,
): string {
  if (!baseUrl) {
    return "";
  }
  const trimSuffixes = resolveProviderBaseUrlPolicy(provider).trimSuffixes;
  return trimSuffixes.reduce((normalized, suffix) => {
    const escapedSuffix = suffix.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
    return normalized.replace(new RegExp(`${escapedSuffix}/?$`, "u"), "");
  }, baseUrl);
}

export function shouldRenderBaseUrlInput(provider: (CredentialPolicyInput & Pick<BaseUrlPolicyInput, "defaultBaseUrl">) | null | undefined): boolean {
  if (!provider) {
    return false;
  }
  const credentialPolicy = resolveProviderCredentialPolicy(provider);
  return credentialPolicy.baseUrlRequired || Boolean(provider.defaultBaseUrl);
}

export function canEditProviderBaseUrl(provider: Pick<BaseUrlPolicyInput, "credentialSource" | "baseUrlPolicy"> | null | undefined): boolean {
  if (!provider || ("credentialSource" in provider && provider.credentialSource === "env")) {
    return false;
  }
  return resolveProviderBaseUrlPolicy(provider).allowOverride;
}

export function shouldRefreshCredentialModels(provider: Pick<AIProvider, "modelDiscovery"> | null | undefined): boolean {
  if (!provider) {
    return false;
  }
  return resolveProviderModelDiscovery(provider).mode === "credential";
}

export function buildBootstrapAiSetup(
  bundle: BootstrapProviderPresetBundleInput | null,
  apiKey: string,
  baseUrl: string,
): BootstrapAiSetupInput | undefined {
  if (!bundle || bundle.credentialSource === "env") {
    return undefined;
  }

  const normalizedApiKey = apiKey.trim();
  const normalizedBaseUrl = normalizeProviderBaseUrl(bundle, baseUrl.trim());

  return {
    providerKind: bundle.providerKind,
    ...(normalizedApiKey ? { apiKey: normalizedApiKey } : {}),
    ...(normalizedBaseUrl ? { baseUrl: normalizedBaseUrl } : {}),
  };
}
