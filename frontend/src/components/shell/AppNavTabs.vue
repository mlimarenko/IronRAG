<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import { useI18n } from 'vue-i18n'

const { t } = useI18n()
const route = useRoute()

const tabs = [
  { to: '/', labelKey: 'shell.home' },
  { to: '/documents', labelKey: 'shell.documents' },
  { to: '/graph', labelKey: 'shell.graph' },
  { to: '/assistant', labelKey: 'shell.assistant' },
  { to: '/admin', labelKey: 'shell.admin' },
  { to: '/swagger', labelKey: 'shell.swagger' },
] as const

const currentTab = computed(
  () =>
    tabs.find((tab) => {
      if (tab.to === '/') {
        return route.path === '/'
      }
      return route.path === tab.to || route.path.startsWith(`${tab.to}/`)
    }) ?? tabs[0],
)
</script>

<template>
  <div class="rr-nav-switcher">
    <nav class="rr-nav-tabs rr-nav-tabs--desktop">
      <RouterLink
        v-for="tab in tabs"
        :key="tab.to"
        class="rr-nav-tabs__link"
        :to="tab.to"
        exact-active-class="is-active"
      >
        {{ t(tab.labelKey) }}
      </RouterLink>
    </nav>

    <q-btn flat no-caps dense class="rr-nav-menu" :aria-label="$t('shell.navigation')">
      <span class="rr-nav-menu__glyph" aria-hidden="true">
        <span />
        <span />
        <span />
      </span>
      <span class="rr-nav-menu__label">{{ t(currentTab.labelKey) }}</span>

      <q-menu auto-close anchor="bottom left" self="top left" class="rr-nav-menu__popup">
        <q-list dense padding class="rr-nav-menu__list">
          <q-item
            v-for="tab in tabs"
            :key="tab.to"
            clickable
            exact
            :to="tab.to"
            class="rr-nav-menu__item"
            :active="currentTab.to === tab.to"
            active-class="is-active"
          >
            <q-item-section>{{ t(tab.labelKey) }}</q-item-section>
          </q-item>
        </q-list>
      </q-menu>
    </q-btn>
  </div>
</template>

<style scoped lang="scss">
.rr-nav-switcher {
  display: flex;
  align-items: center;
  min-width: 0;
}

.rr-nav-menu {
  display: none;
}

.rr-nav-menu :deep(.q-btn__content) {
  gap: 0.5rem;
}

.rr-nav-menu__glyph {
  display: inline-grid;
  gap: 0.18rem;
}

.rr-nav-menu__glyph span {
  display: block;
  width: 0.78rem;
  height: 2px;
  border-radius: 999px;
  background: currentColor;
}

.rr-nav-menu__label {
  font-weight: 700;
}

.rr-nav-menu__popup {
  min-width: 11rem;
  border: 1px solid rgba(226, 232, 240, 0.92);
  border-radius: 16px;
  background: rgba(255, 255, 255, 0.98);
  box-shadow: 0 18px 40px rgba(15, 23, 42, 0.12);
}

.rr-nav-menu__list {
  padding: 0.35rem;
}

.rr-nav-menu__item {
  min-height: 2.5rem;
  border-radius: 12px;
  color: var(--rr-text-secondary);
  font-size: 0.9rem;
  font-weight: 600;
}

.rr-nav-menu__item.is-active,
.rr-nav-menu__item.q-item--active {
  background: rgba(56, 87, 255, 0.08);
  color: var(--rr-text-primary);
}

@media (max-width: 480px) {
  .rr-nav-tabs--desktop {
    display: none;
  }

  .rr-nav-menu {
    display: inline-flex;
    min-height: 2.25rem;
    padding: 0 0.78rem;
    border: 1px solid rgba(203, 213, 225, 0.86);
    border-radius: 999px;
    background: rgba(255, 255, 255, 0.9);
    color: var(--rr-text-primary);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.82);
  }
}
</style>
