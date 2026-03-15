<script setup lang="ts">
import ComponentInventoryTable from 'src/components/design-system/ComponentInventoryTable.vue'
import DesignTokenSwatch from 'src/components/design-system/DesignTokenSwatch.vue'
import PageSection from 'src/components/shell/PageSection.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
import ErrorStateCard from 'src/components/state/ErrorStateCard.vue'
import LoadingSkeletonPanel from 'src/components/state/LoadingSkeletonPanel.vue'
import AppPanel from 'src/components/ui/AppPanel.vue'
import StatusBanner from 'src/components/ui/StatusBanner.vue'

import {
  colorTokens,
  inventoryRows,
  principles,
  spacingTokens,
  workflowSteps,
} from './design-system/design-system-content'
</script>

<template>
  <div class="rr-page-grid">
    <PageSection
      eyebrow="Foundation"
      title="Design system proposal"
      description="Practical in-repo foundation for the RustRAG refactor: visual language, reusable primitives, state patterns, and a path to Storybook-like workflows without waiting on a heavier setup."
      status="draft"
      status-label="Foundation route"
    >
      <div class="rr-grid rr-grid--cards">
        <AppPanel v-for="principle in principles" :key="principle">
          <p>{{ principle }}</p>
        </AppPanel>
      </div>
    </PageSection>

    <PageSection
      eyebrow="Tokens"
      title="Visual language"
      description="Start with CSS custom properties so the shell, pages, and future component stories all read from the same source of truth."
    >
      <div class="rr-grid rr-grid--cards">
        <DesignTokenSwatch
          v-for="token in colorTokens"
          :key="token.token"
          :label="token.label"
          :token="token.token"
          :value="token.value"
          :text-color="token.textColor"
        />
      </div>

      <AppPanel tone="muted" eyebrow="Spacing" title="Spacing rhythm">
        <p class="rr-kicker">Spacing rhythm</p>
        <div class="token-chip-row">
          <span
            v-for="token in spacingTokens"
            :key="token"
            class="token-chip"
          >
            {{ token }}
          </span>
        </div>
      </AppPanel>
    </PageSection>

    <PageSection
      eyebrow="Patterns"
      title="Foundation primitives in use"
      description="The goal is not just token storage. The live route should prove headers, panels, buttons, form rows, banners, and empty states all share one foundation layer."
    >
      <div class="rr-grid rr-grid--two">
        <AppPanel
          eyebrow="Panel"
          title="Surface and actions"
          description="Shared panels now cover title rows, muted/accent surfaces, and action layouts."
          tone="accent"
          status="ready"
          status-label="Reusable now"
        >
          <div class="rr-action-row">
            <button type="button" class="rr-button">
              Primary action
            </button>
            <button type="button" class="rr-button rr-button--secondary">
              Secondary action
            </button>
            <button type="button" class="rr-button rr-button--ghost">
              Ghost action
            </button>
          </div>

          <StatusBanner
            tone="info"
            title="Status banners"
            message="Success, warning, error, and info now read from the same reusable banner pattern."
          />
        </AppPanel>

        <AppPanel
          eyebrow="Forms"
          title="Field and row rhythm"
          description="Dense forms stay aligned when new views reuse the shared row and field classes."
        >
          <div class="rr-form-row rr-form-row--two">
            <label class="rr-field">
              <span class="rr-field__label">Workspace name</span>
              <input type="text" class="rr-control" value="Acme knowledge base">
            </label>

            <label class="rr-field">
              <span class="rr-field__label">Workspace slug</span>
              <input type="text" class="rr-control" value="acme-kb">
            </label>
          </div>

          <label class="rr-field">
            <span class="rr-field__label">Operator note</span>
            <textarea class="rr-control" rows="3">Foundation should keep future product pages aligned.</textarea>
            <span class="rr-field__hint">Hint text belongs to the shared field primitive too.</span>
          </label>
        </AppPanel>
      </div>
    </PageSection>

    <PageSection
      eyebrow="States"
      title="Canonical async and empty states"
      description="These should replace ad hoc text blocks across workspace, project, provider, and ingestion views."
    >
      <div class="rr-grid rr-grid--cards states-grid">
        <LoadingSkeletonPanel title="Loading projects" />
        <EmptyStateCard
          title="No sources connected"
          message="This project has no registered ingestion sources yet."
          hint="Start with a repository, S3 bucket, or document upload and show the expected next action inline."
        />
        <ErrorStateCard
          title="Provider credentials invalid"
          message="Model profile validation failed before ingestion started."
          detail="Expose the failing provider, the last check time, and a safe retry action instead of dumping raw JSON."
        />
      </div>
    </PageSection>

    <PageSection
      eyebrow="Inventory"
      title="Reusable primitives roadmap"
      description="This is the minimum component inventory worth standardizing before the big refactor turns into a CSS junk drawer."
    >
      <AppPanel>
        <ComponentInventoryTable :rows="inventoryRows" />
      </AppPanel>
    </PageSection>

    <PageSection
      eyebrow="Workflow"
      title="Storybook-like path without Storybook lock-in"
      description="The cheap, sane path: keep the foundation in-repo now, then graduate to isolated stories once the primitives settle."
    >
      <div class="rr-grid rr-grid--cards">
        <AppPanel v-for="step in workflowSteps" :key="step">
          <p>{{ step }}</p>
        </AppPanel>
      </div>
    </PageSection>
  </div>
</template>

<style scoped>
.states-grid {
  align-items: start;
}

.token-chip-row {
  display: flex;
  flex-wrap: wrap;
  gap: var(--rr-space-3);
  margin-top: var(--rr-space-3);
}

.token-chip {
  display: inline-flex;
  align-items: center;
  min-height: 36px;
  padding: 0 var(--rr-space-4);
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-pill);
  background: var(--rr-color-bg-surface);
  color: var(--rr-color-text-secondary);
  font-weight: 600;
}

.rr-panel p {
  margin: 0;
  color: var(--rr-color-text-secondary);
}
</style>
