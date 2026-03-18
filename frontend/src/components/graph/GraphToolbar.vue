<script setup lang="ts">
import type {
  GraphNodeType,
  GraphSearchHit,
} from 'src/models/ui/graph'

const props = defineProps<{
  query: string
  filter: GraphNodeType | ''
  hits: GraphSearchHit[]
}>()

const emit = defineEmits<{
  updateQuery: [value: string]
  updateFilter: [value: GraphNodeType | '']
  selectHit: [id: string]
}>()
</script>

<template>
  <div class="rr-graph-toolbar">
    <div class="rr-graph-toolbar__search">
      <input
        :value="props.query"
        type="search"
        :placeholder="$t('graph.search')"
        @input="emit('updateQuery', ($event.target as HTMLInputElement).value)"
      >
      <div
        v-if="props.hits.length"
        class="rr-graph-toolbar__hits"
      >
        <button
          v-for="hit in props.hits"
          :key="hit.id"
          class="rr-graph-toolbar__hit"
          type="button"
          @click="emit('selectHit', hit.id)"
        >
          <strong>{{ hit.label }}</strong>
          <span>{{ $t(`graph.nodeTypes.${hit.nodeType}`) }}</span>
        </button>
      </div>
    </div>

    <select
      :value="props.filter"
      @change="emit('updateFilter', ($event.target as HTMLSelectElement).value as GraphNodeType | '')"
    >
      <option value="">{{ $t('graph.allNodeTypes') }}</option>
      <option value="document">{{ $t('graph.nodeTypes.document') }}</option>
      <option value="entity">{{ $t('graph.nodeTypes.entity') }}</option>
      <option value="topic">{{ $t('graph.nodeTypes.topic') }}</option>
    </select>
  </div>
</template>
