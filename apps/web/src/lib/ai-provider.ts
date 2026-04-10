type BootstrapProviderPresetBundleInput = {
  providerKind: string;
  credentialSource: string;
};

export type BootstrapAiSetupInput = {
  providerKind: string;
  apiKey?: string;
  baseUrl?: string;
};

export function baseUrlForProviderInput(providerKind: string, baseUrl?: string | null): string {
  if (!baseUrl) {
    return "";
  }
  if (providerKind === "ollama") {
    return baseUrl.replace(/\/(?:api|v1)\/?$/u, "");
  }
  return baseUrl;
}

export function hasStoredApiKeySummary(apiKeySummary?: string | null): boolean {
  return Boolean(apiKeySummary && apiKeySummary !== "not_configured");
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
  const normalizedBaseUrl = baseUrl.trim();

  return {
    providerKind: bundle.providerKind,
    apiKey: normalizedApiKey || undefined,
    baseUrl: normalizedBaseUrl || undefined,
  };
}
