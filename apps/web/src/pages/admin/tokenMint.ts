import type { MintTokenRequest } from '@/api/admin';

export type TokenScope = 'workspace' | 'library';

export interface BuildMintTokenRequestInput {
  label: string;
  expiryDays: string;
  scope: TokenScope;
  workspaceId?: string;
  libraryIds: string[];
  permissionKinds: string[];
}

export function resolveTokenExpiry(expiryDays: string): string | undefined {
  if (expiryDays === 'never') {
    return undefined;
  }
  const days = Number.parseInt(expiryDays, 10);
  if (!Number.isFinite(days) || days <= 0) {
    return undefined;
  }
  const expiresAt = new Date();
  expiresAt.setUTCDate(expiresAt.getUTCDate() + days);
  return expiresAt.toISOString();
}

export function buildMintTokenRequest(input: BuildMintTokenRequestInput): MintTokenRequest {
  return {
    label: input.label.trim(),
    workspaceId: input.workspaceId,
    expiresAt: resolveTokenExpiry(input.expiryDays),
    libraryIds: input.scope === 'library' ? input.libraryIds : [],
    permissionKinds: input.permissionKinds,
  };
}
