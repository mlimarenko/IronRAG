<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink, useRoute } from 'vue-router'

import { shellNavItems, type ShellNavItem } from './shellNavigation'

const route = useRoute()
const { t } = useI18n()

const primaryItems = computed(() => shellNavItems.filter((item) => item.emphasis === 'primary'))
const secondaryItems = computed(() => shellNavItems.filter((item) => item.emphasis !== 'primary'))
const activePath = computed(() => route.path)

function isActive(item: ShellNavItem) {
  return (
    activePath.value === item.to ||
    activePath.value.startsWith(`${item.to}/`) ||
    Boolean(item.legacyTo && (activePath.value === item.legacyTo || activePath.value.startsWith(`${item.legacyTo}/`)))
  )
}
</script>

<template>
  <nav class="app-mobile-nav" :aria-label="t('shell.mobileNav.primary')">
    <RouterLink
      v-for="item in primaryItems"
      :key="item.to"
      :to="item.to"
      class="app-mobile-nav__item"
      :data-active="isActive(item)"
      :aria-current="isActive(item) ? 'page' : undefined"
    >
      <span class="app-mobile-nav__step">{{ item.step }}</span>
      <span class="app-mobile-nav__label">{{ t(`shell.nav.items.${item.key}.label`) }}</span>
    </RouterLink>

    <details class="app-mobile-nav__more">
      <summary>
        <span>{{ t('shell.mobileNav.more') }}</span>
      </summary>
      <div class="app-mobile-nav__more-sheet">
        <RouterLink
          v-for="item in secondaryItems"
          :key="item.to"
          :to="item.to"
          class="app-mobile-nav__secondary"
          :data-active="isActive(item)"
          :aria-current="isActive(item) ? 'page' : undefined"
        >
          <div>
            <strong>{{ t(`shell.nav.items.${item.key}.label`) }}</strong>
            <p>{{ t(`shell.nav.items.${item.key}.hint`) }}</p>
          </div>
          <span>{{ item.step }}</span>
        </RouterLink>
      </div>
    </details>
  </nav>
</template>

<style scoped>
.app-mobile-nav {
  position: sticky;
  bottom: 0;
  z-index: 30;
  display: none;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 0.65rem;
  padding: 0.85rem 0.9rem calc(0.85rem + env(safe-area-inset-bottom, 0px));
  border-top: 1px solid rgb(15 23 42 / 0.08);
  background: rgb(255 255 255 / 0.95);
  backdrop-filter: blur(18px);
  box-shadow: 0 -16px 40px rgb(15 23 42 / 0.08);
}

.app-mobile-nav__item,
.app-mobile-nav__more summary,
.app-mobile-nav__secondary {
  text-decoration: none;
}

.app-mobile-nav__item,
.app-mobile-nav__more summary {
  display: grid;
  justify-items: center;
  gap: 0.28rem;
  min-height: 58px;
  padding: 0.55rem 0.45rem;
  border-radius: 1rem;
  border: 1px solid transparent;
  color: var(--rr-color-text-secondary);
  background: transparent;
  cursor: pointer;
  list-style: none;
}

.app-mobile-nav__more summary::-webkit-details-marker {
  display: none;
}

.app-mobile-nav__item[data-active='true'],
.app-mobile-nav__more[open] summary {
  border-color: rgb(59 130 246 / 0.16);
  background: rgb(239 246 255 / 0.92);
  color: var(--rr-color-accent-700);
}

.app-mobile-nav__step {
  display: inline-grid;
  place-items: center;
  min-width: 2rem;
  min-height: 1.6rem;
  padding: 0 0.45rem;
  border-radius: 999px;
  background: rgb(15 23 42 / 0.06);
  font-size: 0.72rem;
  font-weight: 800;
  letter-spacing: 0.08em;
}

.app-mobile-nav__label,
.app-mobile-nav__more summary span {
  font-size: 0.76rem;
  font-weight: 700;
  text-align: center;
}

.app-mobile-nav__more {
  position: relative;
}

.app-mobile-nav__more-sheet {
  position: absolute;
  right: 0;
  bottom: calc(100% + 0.75rem);
  display: grid;
  gap: 0.6rem;
  width: min(280px, calc(100vw - 1.2rem));
  padding: 0.8rem;
  border-radius: 1.1rem;
  border: 1px solid rgb(15 23 42 / 0.08);
  background: rgb(255 255 255 / 0.98);
  box-shadow: 0 18px 44px rgb(15 23 42 / 0.14);
}

.app-mobile-nav__secondary {
  display: flex;
  justify-content: space-between;
  gap: 0.75rem;
  align-items: flex-start;
  padding: 0.8rem 0.9rem;
  border-radius: 0.95rem;
  border: 1px solid rgb(15 23 42 / 0.08);
  color: var(--rr-color-text-primary);
  background: rgb(248 250 252 / 0.88);
}

.app-mobile-nav__secondary[data-active='true'] {
  border-color: rgb(59 130 246 / 0.16);
  background: rgb(239 246 255 / 0.92);
}

.app-mobile-nav__secondary strong,
.app-mobile-nav__secondary p {
  margin: 0;
}

.app-mobile-nav__secondary p {
  margin-top: 0.22rem;
  font-size: 0.82rem;
  color: var(--rr-color-text-secondary);
}

.app-mobile-nav__secondary span {
  font-size: 0.72rem;
  font-weight: 800;
  letter-spacing: 0.08em;
  color: var(--rr-color-text-muted);
}

@media (width <= 900px) {
  .app-mobile-nav {
    display: grid;
  }
}
</style>
