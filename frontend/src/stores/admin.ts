import { defineStore } from 'pinia'
import type {
  AdminMemberRow,
  AdminOverviewResponse,
  AdminPricingCatalogEntry,
  AdminUpsertPricingEntryPayload,
  AdminProviderProfile,
  AdminProviderValidation,
  AdminSettingsResponse,
  AdminTab,
  ApiTokenRow,
  CreateApiTokenPayload,
  LibraryAccessRow,
  UpdateAdminProviderProfilePayload,
} from 'src/models/ui/admin'
import {
  createAdminApiToken,
  createAdminPricingEntry,
  deactivateAdminPricingEntry,
  fetchAdminApiTokens,
  fetchAdminLibraryAccess,
  fetchAdminMembers,
  fetchAdminOverview,
  fetchAdminSettings,
  revokeAdminApiToken,
  updateAdminPricingEntry,
  updateAdminProviderProfile,
  validateAdminProviderProfile,
} from 'src/services/api/admin'

interface AdminState {
  overview: AdminOverviewResponse | null
  activeTab: AdminTab
  loading: boolean
  tabLoading: boolean
  error: string | null
  tokens: ApiTokenRow[]
  members: AdminMemberRow[]
  libraryAccess: LibraryAccessRow[]
  settings: AdminSettingsResponse | null
  settingsSaving: boolean
  settingsValidating: boolean
  pricingSaving: boolean
  showCreateToken: boolean
  latestPlaintextToken: string | null
}

export const useAdminStore = defineStore('admin', {
  state: (): AdminState => ({
    overview: null,
    activeTab: 'api_tokens',
    loading: false,
    tabLoading: false,
    error: null,
    tokens: [],
    members: [],
    libraryAccess: [],
    settings: null,
    settingsSaving: false,
    settingsValidating: false,
    pricingSaving: false,
    showCreateToken: false,
    latestPlaintextToken: null,
  }),
  actions: {
    async loadOverview(): Promise<void> {
      this.loading = true
      this.error = null
      try {
        this.overview = await fetchAdminOverview()
        this.activeTab = this.overview.activeTab
        await this.loadActiveTab()
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to load admin overview'
        throw error
      } finally {
        this.loading = false
      }
    },
    async loadActiveTab(): Promise<void> {
      this.tabLoading = true
      this.error = null
      try {
        if (this.activeTab === 'api_tokens') {
          this.tokens = await fetchAdminApiTokens()
        } else if (this.activeTab === 'members') {
          this.members = await fetchAdminMembers()
        } else if (this.activeTab === 'library_access') {
          this.libraryAccess = await fetchAdminLibraryAccess()
        } else {
          await this.reloadSettings()
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
      this.tokens = [result.row, ...this.tokens]
      this.showCreateToken = true
      if (this.overview) {
        this.overview = {
          ...this.overview,
          counts: {
            ...this.overview.counts,
            apiTokens: this.overview.counts.apiTokens + 1,
          },
        }
      }
    },
    async revokeToken(id: string): Promise<void> {
      const revoked = await revokeAdminApiToken(id)
      this.tokens = this.tokens.map((row) => (row.id === id ? { ...revoked, plaintextToken: null } : row))
    },
    async copyToken(id: string): Promise<void> {
      const row = this.tokens.find((item) => item.id === id)
      if (!row?.plaintextToken) {
        return
      }
      await navigator.clipboard.writeText(row.plaintextToken)
    },
    clearLatestPlaintextToken(): void {
      this.latestPlaintextToken = null
    },
    async saveProviderProfile(payload: UpdateAdminProviderProfilePayload): Promise<void> {
      this.settingsSaving = true
      this.error = null
      try {
        const profile = await updateAdminProviderProfile(payload)
        this.applyProviderProfile(profile)
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to save provider profile'
        throw error
      } finally {
        this.settingsSaving = false
      }
    },
    async validateProviderProfile(): Promise<void> {
      this.settingsValidating = true
      this.error = null
      try {
        const result = await validateAdminProviderProfile()
        this.applyProviderProfile(result.profile)
        this.applyProviderValidation(result.validation)
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to validate provider profile'
        throw error
      } finally {
        this.settingsValidating = false
      }
    },
    async reloadSettings(): Promise<void> {
      this.settings = null
      this.settings = await fetchAdminSettings()
    },
    applyProviderProfile(profile: AdminProviderProfile): void {
      if (!this.settings) {
        return
      }
      this.settings = {
        ...this.settings,
        providerProfile: profile,
        providerValidation: {
          ...this.settings.providerValidation,
          status: profile.lastValidationStatus,
          checkedAt: profile.lastValidatedAt,
          error: profile.lastValidationError,
          checks: profile.lastValidationStatus ? this.settings.providerValidation.checks : [],
        },
      }
    },
    applyProviderValidation(validation: AdminProviderValidation): void {
      if (!this.settings) {
        return
      }
      this.settings = {
        ...this.settings,
        providerValidation: validation,
      }
    },
    async createPricingEntry(payload: AdminUpsertPricingEntryPayload): Promise<AdminPricingCatalogEntry> {
      this.pricingSaving = true
      this.error = null
      try {
        const row = await createAdminPricingEntry(payload)
        await this.reloadSettings()
        return row
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to create pricing entry'
        throw error
      } finally {
        this.pricingSaving = false
      }
    },
    async updatePricingEntry(
      pricingId: string,
      payload: AdminUpsertPricingEntryPayload,
    ): Promise<AdminPricingCatalogEntry> {
      this.pricingSaving = true
      this.error = null
      try {
        const row = await updateAdminPricingEntry(pricingId, payload)
        await this.reloadSettings()
        return row
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to update pricing entry'
        throw error
      } finally {
        this.pricingSaving = false
      }
    },
    async deactivatePricingEntry(pricingId: string): Promise<AdminPricingCatalogEntry> {
      this.pricingSaving = true
      this.error = null
      try {
        const row = await deactivateAdminPricingEntry(pricingId)
        await this.reloadSettings()
        return row
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to deactivate pricing entry'
        throw error
      } finally {
        this.pricingSaving = false
      }
    },
  },
})
