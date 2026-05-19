import { describe, expect, it } from "vitest";

import {
  classifyWebIngestSeedHost,
  isSubdomainBoundaryAvailableForSeed,
  isWebBoundaryPolicy,
  normalizeWebIngestSeedUrlInput,
} from "./webIngestBoundary";

describe("web ingest boundary helpers", () => {
  it("normalizes seed input with an implicit HTTPS scheme", () => {
    expect(normalizeWebIngestSeedUrlInput("docs.example.com/start")).toBe(
      "https://docs.example.com/start",
    );
  });

  it("recognizes the canonical subdomain boundary policy", () => {
    expect(isWebBoundaryPolicy("same_host_and_subdomains")).toBe(true);
    expect(isWebBoundaryPolicy("same_domain")).toBe(false);
  });

  it("marks subdomain boundary unavailable for IP seed hosts", () => {
    expect(classifyWebIngestSeedHost("192.0.2.10/docs")).toBe("ip");
    expect(classifyWebIngestSeedHost("http://[2001:db8::1]/docs")).toBe("ip");
    expect(isSubdomainBoundaryAvailableForSeed("192.0.2.10/docs")).toBe(false);
    expect(isSubdomainBoundaryAvailableForSeed("docs.example.com")).toBe(true);
  });
});
