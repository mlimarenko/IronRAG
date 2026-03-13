<script setup lang="ts">
defineProps<{
  title: string
  emptyMessage: string
  items: string[]
}>()
</script>

<template>
  <div class="section-block">
    <h4>{{ title }}</h4>
    <p
      v-if="!items.length"
      class="muted"
    >
      {{ emptyMessage }}
    </p>
    <ul
      v-else
      class="token-list"
    >
      <li
        v-for="item in items"
        :key="item"
      >
        <code>{{ item }}</code>
      </li>
    </ul>
  </div>
</template>

<style scoped>
.section-block {
  padding: 12px;
  border-radius: 10px;
  background: rgb(255 255 255 / 65%);
}

.muted {
  color: #526173;
}

.token-list {
  display: grid;
  gap: 8px;
  padding-left: 20px;
  margin: 0;
}

.token-list code {
  overflow-wrap: anywhere;
}
</style>
