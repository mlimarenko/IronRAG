<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  initials: string
  displayName: string
  accessLabel?: string
}>()

const emit = defineEmits<{
  logout: []
}>()

const { t } = useI18n()
const rootRef = ref<HTMLElement | null>(null)
const open = ref(false)

const localizedAccessLabel = computed(() => {
  switch (props.accessLabel) {
    case 'Admin access':
      return t('shell.access.admin')
    case 'Write access':
      return t('shell.access.write')
    case 'Read access':
      return t('shell.access.read')
    default:
      return props.accessLabel ?? ''
  }
})

function closeMenu() {
  open.value = false
}

function toggleMenu() {
  open.value = !open.value
}

function handlePointerDown(event: Event) {
  if (!rootRef.value) {
    return
  }
  if (!rootRef.value.contains(event.target as Node)) {
    closeMenu()
  }
}

function handleKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') {
    closeMenu()
  }
}

onMounted(() => {
  document.addEventListener('pointerdown', handlePointerDown)
  document.addEventListener('keydown', handleKeydown)
})

onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', handlePointerDown)
  document.removeEventListener('keydown', handleKeydown)
})
</script>

<template>
  <div
    ref="rootRef"
    class="rr-user-menu"
    :class="{ 'is-open': open }"
  >
    <button
      class="rr-user-menu__trigger"
      type="button"
      aria-haspopup="menu"
      :aria-expanded="open"
      :title="displayName"
      @click="toggleMenu"
    >
      <span class="rr-user-menu__avatar">{{ initials }}</span>
      <span class="rr-user-menu__summary">
        <strong>{{ displayName }}</strong>
      </span>
      <span class="rr-user-menu__chevron">▾</span>
    </button>

    <div
      v-if="open"
      class="rr-user-menu__menu"
      role="menu"
    >
      <div class="rr-user-menu__identity">
        <span class="rr-user-menu__avatar rr-user-menu__avatar--large">{{ initials }}</span>
        <div class="rr-user-menu__summary rr-user-menu__summary--menu">
          <strong>{{ displayName }}</strong>
          <span v-if="localizedAccessLabel">{{ localizedAccessLabel }}</span>
        </div>
      </div>

      <button
        class="rr-user-menu__action"
        type="button"
        role="menuitem"
        @click="emit('logout'); closeMenu()"
      >
        {{ t('shell.logout') }}
      </button>
    </div>
  </div>
</template>
