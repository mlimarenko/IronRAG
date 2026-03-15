<script setup lang="ts">
export interface InventoryRow {
  primitive: string
  purpose: string
  states: string
  nextStep: string
}

defineProps<{
  rows: InventoryRow[]
}>()
</script>

<template>
  <div class="inventory-table">
    <table>
      <thead>
        <tr>
          <th>Primitive</th>
          <th>Purpose</th>
          <th>States to support</th>
          <th>Next step</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="row in rows" :key="row.primitive">
          <th scope="row">{{ row.primitive }}</th>
          <td>{{ row.purpose }}</td>
          <td>{{ row.states }}</td>
          <td>{{ row.nextStep }}</td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<style scoped>
.inventory-table {
  overflow-x: auto;
}

table {
  width: 100%;
  border-collapse: collapse;
  min-width: 720px;
}

th,
td {
  padding: 14px 12px;
  border-bottom: 1px solid var(--rr-color-border-subtle);
  text-align: left;
  vertical-align: top;
}

thead th {
  color: var(--rr-color-text-muted);
  font-size: 0.85rem;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

tbody th {
  width: 19%;
}
</style>
