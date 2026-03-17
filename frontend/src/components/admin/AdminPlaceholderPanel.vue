<script setup lang="ts">
defineProps<{
  title: string
  columns: string[]
  rows: string[][]
}>()
</script>

<template>
  <section class="rr-page-card rr-admin-table">
    <div class="rr-admin-placeholder__header">
      <h3>{{ title }}</h3>
    </div>
    <table>
      <thead>
        <tr>
          <th
            v-for="column in columns"
            :key="column"
          >
            {{ column }}
          </th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="row in rows"
          :key="row.join(':')"
        >
          <td
            v-for="cell in row"
            :key="cell"
          >
            {{ cell }}
          </td>
        </tr>
      </tbody>
    </table>
  </section>
</template>
