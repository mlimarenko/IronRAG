<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type { LibraryOption, WorkspaceOption } from 'src/models/ui/shell'

const props = withDefaults(defineProps<{
  label: string
  selectedId: string
  options: (WorkspaceOption | LibraryOption)[]
  disabled?: boolean
  placeholder?: string
  canCreate?: boolean
  createLabel?: string
  compact?: boolean
}>(), {
  compact: false,
})

const emit = defineEmits<{
  change: [value: string]
  create: []
}>()

const rootRef = ref<HTMLElement | null>(null)
const triggerRef = ref<HTMLButtonElement | null>(null)
const open = ref(false)
const activeIndex = ref(-1)
const optionRefs = ref<Array<HTMLButtonElement | null>>([])

const selectedOption = computed(
  () => props.options.find((option) => option.id === props.selectedId) ?? null,
)

const displayValue = computed(
  () => selectedOption.value?.name ?? props.placeholder ?? props.label,
)

function closeMenu(options?: { restoreFocus?: boolean }) {
  open.value = false
  activeIndex.value = -1
  if (options?.restoreFocus) {
    void nextTick(() => triggerRef.value?.focus())
  }
}

function normalizedStartIndex(): number {
  const selectedIndex = props.options.findIndex((option) => option.id === props.selectedId)
  if (selectedIndex >= 0) {
    return selectedIndex
  }
  return props.options.length > 0 ? 0 : -1
}

function openMenu(startIndex?: number) {
  if (props.disabled) {
    return
  }
  open.value = true
  activeIndex.value = typeof startIndex === 'number' ? startIndex : normalizedStartIndex()
}

function toggleMenu() {
  if (props.disabled) {
    return
  }
  if (open.value) {
    closeMenu({ restoreFocus: true })
    return
  }
  openMenu()
}

function selectOption(id: string) {
  emit('change', id)
  closeMenu({ restoreFocus: true })
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

function focusActiveOption() {
  if (!open.value || activeIndex.value < 0) {
    return
  }
  void nextTick(() => optionRefs.value[activeIndex.value]?.focus())
}

function moveActive(delta: number) {
  if (!props.options.length) {
    return
  }
  const nextIndex =
    activeIndex.value < 0
      ? 0
      : (activeIndex.value + delta + props.options.length) % props.options.length
  activeIndex.value = nextIndex
  focusActiveOption()
}

function handleTriggerKeydown(event: KeyboardEvent) {
  if (props.disabled) {
    return
  }

  if (event.key === 'ArrowDown') {
    event.preventDefault()
    openMenu(normalizedStartIndex())
    focusActiveOption()
    return
  }

  if (event.key === 'ArrowUp') {
    event.preventDefault()
    const startIndex = props.options.length > 0 ? props.options.length - 1 : -1
    openMenu(startIndex)
    focusActiveOption()
    return
  }

  if (event.key === 'Enter' || event.key === ' ') {
    event.preventDefault()
    toggleMenu()
  }
}

function handleMenuKeydown(event: KeyboardEvent) {
  if (!open.value) {
    return
  }

  if (event.key === 'ArrowDown') {
    event.preventDefault()
    moveActive(1)
    return
  }

  if (event.key === 'ArrowUp') {
    event.preventDefault()
    moveActive(-1)
    return
  }

  if (event.key === 'Home') {
    event.preventDefault()
    activeIndex.value = props.options.length > 0 ? 0 : -1
    focusActiveOption()
    return
  }

  if (event.key === 'End') {
    event.preventDefault()
    activeIndex.value = props.options.length > 0 ? props.options.length - 1 : -1
    focusActiveOption()
    return
  }
}

function setOptionRef(element: HTMLButtonElement | null, index: number) {
  optionRefs.value[index] = element
}

watch(open, (isOpen) => {
  if (!isOpen) {
    optionRefs.value = []
    return
  }
  focusActiveOption()
})

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
    class="rr-selector"
    :class="{ 'rr-selector--compact': compact, 'is-open': open }"
  >
    <button
      ref="triggerRef"
      class="rr-selector__trigger"
      type="button"
      aria-haspopup="listbox"
      :aria-expanded="open"
      :aria-label="label"
      :disabled="disabled"
      :title="selectedOption?.name ?? ''"
      @click="toggleMenu"
      @keydown="handleTriggerKeydown"
    >
      <span class="rr-selector__copy">
        <span class="rr-selector__label">{{ label }}</span>
        <span class="rr-selector__value">{{ displayValue }}</span>
      </span>
      <span class="rr-selector__chevron">▾</span>
    </button>

    <div
      v-if="open"
      class="rr-selector__menu"
      role="listbox"
      :aria-label="label"
      @keydown="handleMenuKeydown"
    >
      <button
        v-for="(option, index) in options"
        :key="option.id"
        :ref="(element) => setOptionRef(element as HTMLButtonElement | null, index)"
        class="rr-selector__option"
        :class="{ 'is-active': option.id === selectedId }"
        type="button"
        role="option"
        :aria-selected="option.id === selectedId"
        :tabindex="index === activeIndex ? 0 : -1"
        @click="selectOption(option.id)"
      >
        <span>{{ option.name }}</span>
        <span
          v-if="option.id === selectedId"
          class="rr-selector__tick"
        >
          ✓
        </span>
      </button>

      <p
        v-if="!options.length"
        class="rr-selector__empty"
      >
        {{ placeholder ?? label }}
      </p>

      <div
        v-if="canCreate"
        class="rr-selector__footer"
      >
        <button
          class="rr-selector__create"
          type="button"
          @click="emit('create'); closeMenu()"
        >
          {{ createLabel ?? label }}
        </button>
      </div>
    </div>
  </div>
</template>
