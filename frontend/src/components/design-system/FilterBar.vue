<template>
  <div class="rr-filter-bar">
    <div
      v-if="$slots.search || $slots.filters"
      class="rr-filter-bar__controls"
    >
      <slot name="search" />
      <slot name="filters" />
    </div>
    <div
      v-if="$slots.summary || $slots.actions"
      class="rr-filter-bar__meta"
    >
      <slot name="summary" />
      <slot name="actions" />
    </div>
  </div>
</template>
