import type { WebBoundaryPolicy } from "@/shared/api";

export const WEB_BOUNDARY_POLICIES: readonly WebBoundaryPolicy[] = [
  "same_host",
  "same_host_and_subdomains",
  "allow_external",
];

type WebIngestSeedHostKind = "empty" | "invalid" | "dns" | "ip";

export function isWebBoundaryPolicy(value: string): value is WebBoundaryPolicy {
  return WEB_BOUNDARY_POLICIES.some((policy) => policy === value);
}

export function normalizeWebIngestSeedUrlInput(value: string): string | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const candidate = /^https?:\/\//i.test(trimmed) ? trimmed : `https://${trimmed}`;
  try {
    return new URL(candidate).toString();
  } catch {
    return null;
  }
}

export function classifyWebIngestSeedHost(value: string): WebIngestSeedHostKind {
  const trimmed = value.trim();
  if (!trimmed) return "empty";
  const normalized = normalizeWebIngestSeedUrlInput(trimmed);
  if (!normalized) return "invalid";
  const host = new URL(normalized).hostname.replace(/^\[|\]$/g, "");
  if (host.includes(":") || isIpv4Host(host)) return "ip";
  return "dns";
}

export function isSubdomainBoundaryAvailableForSeed(value: string): boolean {
  return classifyWebIngestSeedHost(value) !== "ip";
}

function isIpv4Host(host: string): boolean {
  const parts = host.split(".");
  return (
    parts.length === 4 &&
    parts.every((part) => {
      if (!/^\d{1,3}$/.test(part)) return false;
      const octet = Number.parseInt(part, 10);
      return octet >= 0 && octet <= 255;
    })
  );
}
