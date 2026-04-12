import { describe, expect, it, vi } from 'vitest';

import { buildMintTokenRequest, resolveTokenExpiry } from './tokenMint';

describe('resolveTokenExpiry', () => {
  it('returns undefined for never', () => {
    expect(resolveTokenExpiry('never')).toBeUndefined();
  });

  it('returns an ISO timestamp for positive day counts', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-04-12T10:00:00.000Z'));

    expect(resolveTokenExpiry('30')).toBe('2026-05-12T10:00:00.000Z');

    vi.useRealTimers();
  });
});

describe('buildMintTokenRequest', () => {
  it('builds a workspace-scoped request without library ids', () => {
    const request = buildMintTokenRequest({
      label: '  Ops token  ',
      expiryDays: '90',
      scope: 'workspace',
      workspaceId: 'ws-1',
      libraryIds: ['lib-1', 'lib-2'],
      permissionKinds: ['workspace_read', 'library_read'],
    });

    expect(request.label).toBe('Ops token');
    expect(request.workspaceId).toBe('ws-1');
    expect(request.libraryIds).toEqual([]);
    expect(request.permissionKinds).toEqual(['workspace_read', 'library_read']);
  });

  it('builds a library-scoped request with all selected libraries', () => {
    const request = buildMintTokenRequest({
      label: 'Library token',
      expiryDays: 'never',
      scope: 'library',
      workspaceId: 'ws-2',
      libraryIds: ['lib-a', 'lib-b'],
      permissionKinds: ['library_read', 'document_read'],
    });

    expect(request.workspaceId).toBe('ws-2');
    expect(request.expiresAt).toBeUndefined();
    expect(request.libraryIds).toEqual(['lib-a', 'lib-b']);
    expect(request.permissionKinds).toEqual(['library_read', 'document_read']);
  });
});
