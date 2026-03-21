import { defineStore } from 'pinia'
import type {
  AdminAiConsoleState,
  AdminApiTokenRow,
  AdminAuditEvent,
  AdminGrant,
  AdminOpsLibrarySnapshot,
  AdminPermissionKind,
  AdminPrincipalSummary,
  AdminTab,
  AdminTabAvailability,
  AdminTabCounts,
  CreateAdminCredentialPayload,
  CreateApiTokenPayload,
} from 'src/models/ui/admin'
import {
  createAdminApiToken,
  createAdminCredential,
  createAdminGrant,
  fetchAdminAiConsole,
  fetchAdminApiTokens,
  fetchAdminAuditEvents,
  fetchAdminLibraryOpsState,
  fetchAdminPrincipal,
  revokeAdminApiToken,
  validateAdminLibraryBinding,
} from 'src/services/api/admin'

interface AdminContext {
  workspaceId: string
  workspaceName: string
  libraryId: string
  libraryName: string
}

interface AdminState {
  activeTab: AdminTab
  loading: boolean
  tabLoading: boolean
  error: string | null
  context: AdminContext | null
  principal: AdminPrincipalSummary | null
  tokens: AdminApiTokenRow[]
  aiConsole: AdminAiConsoleState | null
  aiConsoleContextKey: string | null
  auditEvents: AdminAuditEvent[]
  opsSnapshot: AdminOpsLibrarySnapshot | null
  credentialSaving: boolean
  bindingValidatingId: string | null
  showCreateToken: boolean
  latestPlaintextToken: string | null
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
  'audit_read',
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

function formatGrantFailureMessage(failedPermissions: AdminPermissionKind[]): string {
  return `Token minted, but grant assignment failed for: ${failedPermissions.join(', ')}`
}

export const useAdminStore = defineStore('admin', {
  state: (): AdminState => ({
    activeTab: 'tokens',
    loading: false,
    tabLoading: false,
    error: null,
    context: null,
    principal: null,
    tokens: [],
    aiConsole: null,
    aiConsoleContextKey: null,
    auditEvents: [],
    opsSnapshot: null,
    credentialSaving: false,
    bindingValidatingId: null,
    showCreateToken: false,
    latestPlaintextToken: null,
  }),
  getters: {
    tabCounts(state): AdminTabCounts {
      return {
        tokens: state.tokens.length,
        aiCatalog:
          (state.aiConsole?.providers.length ?? 0) +
          (state.aiConsole?.modelPresets.length ?? 0) +
          (state.aiConsole?.credentials.length ?? 0) +
          (state.aiConsole?.bindings.length ?? 0),
        pricing: state.aiConsole?.prices.length ?? 0,
        audit: state.auditEvents.length,
      }
    },
    tabAvailability(state): AdminTabAvailability {
      const hasPermission = (accepted: AdminPermissionKind[]): boolean =>
        state.principal?.effectiveGrants.some((grant) => {
          if (!accepted.includes(grant.permissionKind)) {
            return false
          }
          if (grant.resourceKind === 'system') {
            return true
          }
          if (grant.resourceKind === 'workspace') {
            return grant.resourceId === state.context?.workspaceId
          }
          if (grant.resourceKind === 'library') {
            return grant.resourceId === state.context?.libraryId
          }
          return false
        }) ?? false

      return {
        tokens: hasPermission(['iam_admin']),
        aiCatalog:
          hasPermission(WORKSPACE_DISCOVERY_PERMISSIONS) ||
          hasPermission(LIBRARY_DISCOVERY_PERMISSIONS),
        pricing:
          hasPermission(WORKSPACE_DISCOVERY_PERMISSIONS) ||
          hasPermission(LIBRARY_DISCOVERY_PERMISSIONS),
        audit: hasPermission(['audit_read']),
      }
    },
  },
  actions: {
    clearState(): void {
      this.error = null
      this.loading = false
      this.tabLoading = false
      this.principal = null
      this.tokens = []
      this.aiConsole = null
      this.aiConsoleContextKey = null
      this.auditEvents = []
      this.opsSnapshot = null
      this.latestPlaintextToken = null
      this.showCreateToken = false
    },
    async loadForContext(context: AdminContext): Promise<void> {
      this.loading = true
      this.error = null
      this.context = context
      try {
        this.principal = await fetchAdminPrincipal()
        this.opsSnapshot = await fetchAdminLibraryOpsState(context.libraryId).catch(() => null)
        if (!this.tabAvailability[this.activeTab]) {
          const firstAvailable = (Object.entries(this.tabAvailability).find(
            ([, available]) => available,
          )?.[0] ?? 'tokens') as AdminTab
          this.activeTab = firstAvailable
        }
        await this.loadActiveTab()
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to load admin state'
        throw error
      } finally {
        this.loading = false
      }
    },
    async loadActiveTab(): Promise<void> {
      if (!this.context) {
        return
      }

      this.tabLoading = true
      this.error = null
      try {
        if (this.activeTab === 'tokens') {
          this.tokens = this.tabAvailability.tokens
            ? await fetchAdminApiTokens(this.context.workspaceId)
            : []
        } else if (this.activeTab === 'aiCatalog' || this.activeTab === 'pricing') {
          if (this.tabAvailability.aiCatalog || this.tabAvailability.pricing) {
            const contextKey = `${this.context.workspaceId}:${this.context.libraryId}`
            if (this.aiConsole !== null && this.aiConsoleContextKey === contextKey) {
              return
            }
            this.aiConsole = await fetchAdminAiConsole(this.context)
            this.aiConsoleContextKey = contextKey
          } else {
            this.aiConsole = null
            this.aiConsoleContextKey = null
          }
        } else {
          this.auditEvents = this.tabAvailability.audit
            ? await fetchAdminAuditEvents({
                workspaceId: this.context.workspaceId,
                libraryId: this.context.libraryId,
              })
            : []
        }
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to load admin tab'
        throw error
      } finally {
        this.tabLoading = false
      }
    },
    async switchTab(tab: AdminTab): Promise<void> {
      this.activeTab = tab
      await this.loadActiveTab()
    },
    async createToken(payload: CreateApiTokenPayload): Promise<void> {
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
        this.error = formatGrantFailureMessage(failedPermissions)
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
      this.credentialSaving = true
      this.error = null
      try {
        const created = await createAdminCredential(payload)
        this.aiConsole = {
          ...this.aiConsole,
          credentials: [created, ...this.aiConsole.credentials],
        }
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to create credential'
        throw error
      } finally {
        this.credentialSaving = false
      }
    },
    async validateBinding(bindingId: string): Promise<void> {
      if (!this.aiConsole) {
        return
      }
      this.bindingValidatingId = bindingId
      this.error = null
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
        this.error = error instanceof Error ? error.message : 'Failed to validate binding'
        throw error
      } finally {
        this.bindingValidatingId = null
      }
    },
  },
})
