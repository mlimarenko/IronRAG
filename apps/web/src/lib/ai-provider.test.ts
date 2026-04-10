import { describe, expect, it } from "vitest";

import {
  baseUrlForProviderInput,
  buildBootstrapAiSetup,
  hasStoredApiKeySummary,
} from "@/lib/ai-provider";

describe("baseUrlForProviderInput", () => {
  it("strips the openai-compatible suffix for ollama inputs", () => {
    expect(baseUrlForProviderInput("ollama", "http://localhost:11434/v1")).toBe(
      "http://localhost:11434",
    );
    expect(baseUrlForProviderInput("ollama", "http://localhost:11434/api")).toBe(
      "http://localhost:11434",
    );
  });

  it("leaves other providers unchanged", () => {
    expect(baseUrlForProviderInput("openai", "https://api.openai.com/v1")).toBe(
      "https://api.openai.com/v1",
    );
  });
});

describe("hasStoredApiKeySummary", () => {
  it("treats missing summaries as absent tokens", () => {
    expect(hasStoredApiKeySummary("not_configured")).toBe(false);
    expect(hasStoredApiKeySummary("")).toBe(false);
  });

  it("keeps masked summaries as configured tokens", () => {
    expect(hasStoredApiKeySummary("sk-1••••1234")).toBe(true);
  });
});

describe("buildBootstrapAiSetup", () => {
  it("keeps openai bootstrap keyed by provider kind", () => {
    expect(
      buildBootstrapAiSetup(
        { providerKind: "openai", credentialSource: "missing" },
        " sk-test ",
        "",
      ),
    ).toEqual({
      providerKind: "openai",
      apiKey: "sk-test",
      baseUrl: undefined,
    });
  });

  it("keeps ollama base url even when token is empty", () => {
    expect(
      buildBootstrapAiSetup(
        { providerKind: "ollama", credentialSource: "missing" },
        "",
        " http://localhost:11434 ",
      ),
    ).toEqual({
      providerKind: "ollama",
      apiKey: undefined,
      baseUrl: "http://localhost:11434",
    });
  });

  it("skips explicit setup when provider is already configured from env", () => {
    expect(
      buildBootstrapAiSetup(
        { providerKind: "openai", credentialSource: "env" },
        "sk-test",
        "",
      ),
    ).toBeUndefined();
  });
});
