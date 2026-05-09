import type { MintTokenRequest } from '@/shared/api/admin';
import type { IamPermissionKind } from '@/shared/api/generated';

type TokenScope = 'system' | 'workspace' | 'library';

interface BuildMintTokenRequestInput {
  label: string;
  expiryDays: string;
  scope: TokenScope;
  workspaceId?: string;
  libraryIds: string[];
  permissionKinds: IamPermissionKind[];
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
  const expiresAt = resolveTokenExpiry(input.expiryDays);
  return {
    label: input.label.trim(),
    ...(input.scope === 'system' ? {} : { workspaceId: input.workspaceId }),
    ...(expiresAt ? { expiresAt } : {}),
    libraryIds: input.scope === 'library' ? input.libraryIds : [],
    permissionKinds: input.permissionKinds,
  };
}
