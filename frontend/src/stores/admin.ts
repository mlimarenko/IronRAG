import { defineStore } from 'pinia'
import type {
  AdminAiConsoleState,
  AdminApiTokenRow,
  AdminAuditEvent,
  AdminGrant,
  AdminPermissionKind,
  AdminPrincipalSummary,
  AdminOpsLibrarySnapshot,
  CreateAdminPricePayload,
  CreateAdminCredentialPayload,
  CreateAdminModelPresetPayload,
  CreateApiTokenPayload,
  SaveAdminLibraryBindingPayload,
  UpdateAdminPricePayload,
  UpdateAdminCredentialPayload,
  UpdateAdminModelPresetPayload,
} from 'src/models/ui/admin'
import {
  createAdminApiToken,
  createAdminPrice,
  createAdminCredential,
  createAdminGrant,
  createAdminModelPreset,
  fetchAdminAiConsole,
  fetchAdminApiTokens,
  fetchAdminAuditEvents,
  fetchAdminLibraryOpsState,
  fetchAdminPrincipal,
  revokeAdminApiToken,
  saveAdminLibraryBinding,
  updateAdminPrice,
  updateAdminCredential,
  updateAdminModelPreset,
  validateAdminLibraryBinding,
} from 'src/services/api/admin'

interface AdminContext {
  workspaceId: string
  workspaceName: string
  libraryId: string
  libraryName: string
}

interface AdminState {
  loading: boolean
  error: string | null
  context: AdminContext | null
  principal: AdminPrincipalSummary | null

  // Access section
  tokens: AdminApiTokenRow[]
  accessSaving: boolean
  accessError: string | null
  showCreateToken: boolean
  latestPlaintextToken: string | null

  // Operations section
  opsSnapshot: AdminOpsLibrarySnapshot | null
  auditEvents: AdminAuditEvent[]

  // AI setup section
  aiConsole: AdminAiConsoleState | null
  aiConsoleContextKey: string | null
  aiSetupSaving: boolean
  aiSetupError: string | null
  bindingValidatingId: string | null

  // Prices section
  pricesSaving: boolean
  pricesError: string | null

  catalogCommitVersion: number
  loadRequestId: number
}

const WORKSPACE_DISCOVERY_PERMISSIONS: AdminPermissionKind[] = [
  'workspace_admin',
  'workspace_read',
  'library_read',
  'library_write',
  'document_read',
  'document_write',
  'credential_admin',
  'binding_admin',
  'query_run',
  'ops_read',
  'iam_admin',
]

const LIBRARY_DISCOVERY_PERMISSIONS: AdminPermissionKind[] = [
  'library_read',
  'library_write',
  'document_read',
  'document_write',
  'binding_admin',
  'query_run',
]

const ACCESS_PERMISSIONS: AdminPermissionKind[] = ['iam_admin']

const OPERATIONS_PERMISSIONS: AdminPermissionKind[] = ['ops_read', 'workspace_admin', 'iam_admin']

const AUDIT_PERMISSIONS: AdminPermissionKind[] = ['audit_read', 'workspace_admin', 'iam_admin']

function formatGrantFailureMessage(failedPermissions: AdminPermissionKind[]): string {
  return `Token minted, but grant assignment failed for: ${failedPermissions.join(', ')}`
}

function hasScopedPermission(
  principal: AdminPrincipalSummary | null,
  context: AdminContext | null,
  accepted: AdminPermissionKind[],
): boolean {
  if (!principal || !context) {
    return false
  }
  return principal.effectiveGrants.some((grant) => {
    if (!accepted.includes(grant.permissionKind)) {
      return false
    }
    if (grant.resourceKind === 'system') {
      return true
    }
    if (grant.resourceKind === 'workspace') {
      return grant.resourceId === context.workspaceId
    }
    if (grant.resourceKind === 'library') {
      return grant.resourceId === context.libraryId
    }
    return false
  })
}

export const useAdminStore = defineStore('admin', {
  state: (): AdminState => ({
    loading: false,
    error: null,
    context: null,
    principal: null,
    tokens: [],
    accessSaving: false,
    accessError: null,
    showCreateToken: false,
    latestPlaintextToken: null,
    opsSnapshot: null,
    auditEvents: [],
    aiConsole: null,
    aiConsoleContextKey: null,
    aiSetupSaving: false,
    aiSetupError: null,
    bindingValidatingId: null,
    pricesSaving: false,
    pricesError: null,
    catalogCommitVersion: 0,
    loadRequestId: 0,
  }),
  getters: {
    canManageAccess(state): boolean {
      return hasScopedPermission(state.principal, state.context, ACCESS_PERMISSIONS)
    },
    canReadOperations(state): boolean {
      return hasScopedPermission(state.principal, state.context, OPERATIONS_PERMISSIONS)
    },
    canReadAudit(state): boolean {
      return hasScopedPermission(state.principal, state.context, AUDIT_PERMISSIONS)
    },
    canManageAi(state): boolean {
      return (
        hasScopedPermission(state.principal, state.context, WORKSPACE_DISCOVERY_PERMISSIONS) ||
        hasScopedPermission(state.principal, state.context, LIBRARY_DISCOVERY_PERMISSIONS)
      )
    },
  },
  actions: {
    clearState(): void {
      this.context = null
      this.error = null
      this.loading = false
      this.principal = null
      this.tokens = []
      this.accessSaving = false
      this.accessError = null
      this.showCreateToken = false
      this.latestPlaintextToken = null
      this.opsSnapshot = null
      this.auditEvents = []
      this.aiConsole = null
      this.aiConsoleContextKey = null
      this.aiSetupSaving = false
      this.aiSetupError = null
      this.bindingValidatingId = null
      this.pricesSaving = false
      this.pricesError = null
      this.catalogCommitVersion = 0
      this.loadRequestId = 0
    },
    async reloadAiConsole(): Promise<void> {
      if (!this.context) {
        return
      }
      const contextKey = `${this.context.workspaceId}:${this.context.libraryId}`
      const nextConsole = await fetchAdminAiConsole(this.context)
      if (`${this.context.workspaceId}:${this.context.libraryId}` !== contextKey) {
        return
      }
      this.aiConsole = nextConsole
      this.aiConsoleContextKey = contextKey
    },
    async loadForContext(context: AdminContext): Promise<void> {
      const requestId = ++this.loadRequestId
      this.loading = true
      this.error = null
      this.accessError = null
      this.aiSetupError = null
      this.pricesError = null
      this.context = context
      try {
        const contextKey = `${context.workspaceId}:${context.libraryId}`
        const principal = await fetchAdminPrincipal()
        const canManageAi = hasScopedPermission(
          principal,
          context,
          WORKSPACE_DISCOVERY_PERMISSIONS,
        ) || hasScopedPermission(principal, context, LIBRARY_DISCOVERY_PERMISSIONS)
        const canManageAccess = hasScopedPermission(principal, context, ACCESS_PERMISSIONS)
        const canReadOperations = hasScopedPermission(principal, context, OPERATIONS_PERMISSIONS)
        const canReadAudit = hasScopedPermission(principal, context, AUDIT_PERMISSIONS)

        this.principal = principal

        const [aiConsole, tokens, opsSnapshot, auditEvents] = await Promise.all([
          canManageAi ? fetchAdminAiConsole(context) : Promise.resolve(null),
          canManageAccess ? fetchAdminApiTokens(context.workspaceId) : Promise.resolve([]),
          canReadOperations
            ? fetchAdminLibraryOpsState(context.libraryId)
            : Promise.resolve(null),
          canReadAudit
            ? fetchAdminAuditEvents({
                workspaceId: context.workspaceId,
                libraryId: context.libraryId,
              })
            : Promise.resolve([]),
        ])

        if (
          this.loadRequestId !== requestId ||
          this.context?.workspaceId !== context.workspaceId ||
          this.context?.libraryId !== context.libraryId
        ) {
          return
        }

        this.aiConsole = aiConsole
        this.aiConsoleContextKey = aiConsole ? contextKey : null
        this.tokens = tokens
        this.opsSnapshot = opsSnapshot
        this.auditEvents = auditEvents
      } catch (error) {
        if (
          this.loadRequestId !== requestId ||
          this.context?.workspaceId !== context.workspaceId ||
          this.context?.libraryId !== context.libraryId
        ) {
          return
        }
        this.error = error instanceof Error ? error.message : 'Failed to load admin state'
        throw error
      } finally {
        if (this.loadRequestId === requestId) {
          this.loading = false
        }
      }
    },
    async createToken(payload: CreateApiTokenPayload): Promise<void> {
      if (!this.canManageAccess) {
        this.accessError = 'Access management is not available in the active context'
        throw new Error(this.accessError)
      }
      this.accessSaving = true
      this.accessError = null
      try {
        const result = await createAdminApiToken(payload)
        this.latestPlaintextToken = result.plaintextToken

        const failedPermissions: AdminPermissionKind[] = []
        const grantedPermissions: AdminGrant[] = []
        for (const permissionKind of payload.permissionKinds) {
          try {
            const createdGrant = await createAdminGrant({
              principalId: result.row.principalId,
              resourceKind: payload.grantResourceKind,
              resourceId: payload.grantResourceId,
              permissionKind,
            })
            grantedPermissions.push(createdGrant)
          } catch {
            failedPermissions.push(permissionKind)
          }
        }

        this.tokens = [{ ...result.row, grants: grantedPermissions }, ...this.tokens]
        this.showCreateToken = true

        if (failedPermissions.length > 0) {
          this.accessError = formatGrantFailureMessage(failedPermissions)
        }
      } catch (error) {
        this.accessError = error instanceof Error ? error.message : 'Failed to create token'
        throw error
      } finally {
        this.accessSaving = false
      }
    },
    async revokeToken(principalId: string): Promise<void> {
      await revokeAdminApiToken(principalId)
      if (!this.context) {
        return
      }
      this.tokens = await fetchAdminApiTokens(this.context.workspaceId)
    },
    async copyToken(principalId: string): Promise<void> {
      const row = this.tokens.find((item) => item.principalId === principalId)
      if (!row?.plaintextToken) {
        return
      }
      await navigator.clipboard.writeText(row.plaintextToken)
    },
    clearLatestPlaintextToken(): void {
      this.latestPlaintextToken = null
    },
    async createCredential(payload: CreateAdminCredentialPayload): Promise<void> {
      if (!this.aiConsole) {
        return
      }
      this.aiSetupSaving = true
      this.aiSetupError = null
      try {
        await createAdminCredential(payload)
        await this.reloadAiConsole()
        this.catalogCommitVersion += 1
      } catch (error) {
        this.aiSetupError = error instanceof Error ? error.message : 'Failed to create credential'
        throw error
      } finally {
        this.aiSetupSaving = false
      }
    },
    async updateCredential(payload: UpdateAdminCredentialPayload): Promise<void> {
      if (!this.aiConsole) {
        return
      }
      this.aiSetupSaving = true
      this.aiSetupError = null
      try {
        await updateAdminCredential(payload)
        await this.reloadAiConsole()
        this.catalogCommitVersion += 1
      } catch (error) {
        this.aiSetupError = error instanceof Error ? error.message : 'Failed to update credential'
        throw error
      } finally {
        this.aiSetupSaving = false
      }
    },
    async createPrice(payload: CreateAdminPricePayload): Promise<void> {
      if (!this.aiConsole) {
        return
      }
      this.pricesSaving = true
      this.pricesError = null
      try {
        await createAdminPrice(payload)
        await this.reloadAiConsole()
        this.catalogCommitVersion += 1
      } catch (error) {
        this.pricesError = error instanceof Error ? error.message : 'Failed to save price'
        throw error
      } finally {
        this.pricesSaving = false
      }
    },
    async updatePrice(payload: UpdateAdminPricePayload): Promise<void> {
      if (!this.aiConsole) {
        return
      }
      this.pricesSaving = true
      this.pricesError = null
      try {
        await updateAdminPrice(payload)
        await this.reloadAiConsole()
        this.catalogCommitVersion += 1
      } catch (error) {
        this.pricesError = error instanceof Error ? error.message : 'Failed to save price'
        throw error
      } finally {
        this.pricesSaving = false
      }
    },
    async createModelPreset(payload: CreateAdminModelPresetPayload): Promise<void> {
      if (!this.aiConsole) {
        return
      }
      this.aiSetupSaving = true
      this.aiSetupError = null
      try {
        await createAdminModelPreset(payload)
        await this.reloadAiConsole()
        this.catalogCommitVersion += 1
      } catch (error) {
        this.aiSetupError = error instanceof Error ? error.message : 'Failed to create model preset'
        throw error
      } finally {
        this.aiSetupSaving = false
      }
    },
    async updateModelPreset(payload: UpdateAdminModelPresetPayload): Promise<void> {
      if (!this.aiConsole) {
        return
      }
      this.aiSetupSaving = true
      this.aiSetupError = null
      try {
        await updateAdminModelPreset(payload)
        await this.reloadAiConsole()
        this.catalogCommitVersion += 1
      } catch (error) {
        this.aiSetupError = error instanceof Error ? error.message : 'Failed to update model preset'
        throw error
      } finally {
        this.aiSetupSaving = false
      }
    },
    async saveBinding(payload: SaveAdminLibraryBindingPayload): Promise<void> {
      if (!this.aiConsole) {
        return
      }
      this.aiSetupSaving = true
      this.aiSetupError = null
      try {
        await saveAdminLibraryBinding(payload)
        await this.reloadAiConsole()
        this.catalogCommitVersion += 1
      } catch (error) {
        this.aiSetupError = error instanceof Error ? error.message : 'Failed to save library binding'
        throw error
      } finally {
        this.aiSetupSaving = false
      }
    },
    async validateBinding(bindingId: string): Promise<void> {
      if (!this.aiConsole) {
        return
      }
      this.bindingValidatingId = bindingId
      this.aiSetupError = null
      try {
        const validation = await validateAdminLibraryBinding(bindingId)
        this.aiConsole = {
          ...this.aiConsole,
          bindings: this.aiConsole.bindings.map((binding) =>
            binding.id === bindingId
              ? { ...binding, latestValidation: validation }
              : binding,
          ),
        }
      } catch (error) {
        this.aiSetupError = error instanceof Error ? error.message : 'Failed to validate binding'
        throw error
      } finally {
        this.bindingValidatingId = null
      }
    },
  },
})
