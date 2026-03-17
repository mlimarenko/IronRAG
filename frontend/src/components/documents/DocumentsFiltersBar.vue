<script setup lang="ts">
import type {
  DocumentAccountingStatus,
  DocumentMutationStatus,
  DocumentStatus,
} from 'src/models/ui/documents'

const props = defineProps<{
  searchQuery: string
  statusFilter: DocumentStatus | ''
  accountingFilter: DocumentAccountingStatus | ''
  mutationStatusFilter: DocumentMutationStatus | ''
  fileTypeFilter: string
  statusOptions: DocumentStatus[]
  accountingOptions: DocumentAccountingStatus[]
  mutationStatusOptions: DocumentMutationStatus[]
  fileTypeOptions: string[]
}>()

const emit = defineEmits<{
  updateSearch: [value: string]
  updateStatus: [value: DocumentStatus | '']
  updateAccounting: [value: DocumentAccountingStatus | '']
  updateMutationStatus: [value: DocumentMutationStatus | '']
  updateFileType: [value: string]
}>()
</script>

<template>
  <section class="rr-documents__filters">
    <input
      :value="props.searchQuery"
      type="search"
      :placeholder="$t('documents.search')"
      @input="emit('updateSearch', ($event.target as HTMLInputElement).value)"
    >
    <select
      :value="props.statusFilter"
      @change="emit('updateStatus', ($event.target as HTMLSelectElement).value as DocumentStatus | '')"
    >
      <option value="">{{ $t('documents.allStatuses') }}</option>
      <option
        v-for="status in props.statusOptions"
        :key="status"
        :value="status"
      >
        {{ $t(`documents.status.${status}`) }}
      </option>
    </select>
    <select
      :value="props.accountingFilter"
      @change="emit('updateAccounting', ($event.target as HTMLSelectElement).value as DocumentAccountingStatus | '')"
    >
      <option value="">{{ $t('documents.allAccounting') }}</option>
      <option
        v-for="accountingStatus in props.accountingOptions"
        :key="accountingStatus"
        :value="accountingStatus"
      >
        {{ $t(`documents.accounting.${accountingStatus}`) }}
      </option>
    </select>
    <select
      :value="props.mutationStatusFilter"
      @change="emit('updateMutationStatus', ($event.target as HTMLSelectElement).value as DocumentMutationStatus | '')"
    >
      <option value="">{{ $t('documents.allMutations') }}</option>
      <option
        v-for="mutationStatus in props.mutationStatusOptions"
        :key="mutationStatus"
        :value="mutationStatus"
      >
        {{ $t(`documents.mutation.status.${mutationStatus}`) }}
      </option>
    </select>
    <select
      :value="props.fileTypeFilter"
      @change="emit('updateFileType', ($event.target as HTMLSelectElement).value)"
    >
      <option value="">{{ $t('documents.allTypes') }}</option>
      <option
        v-for="fileType in props.fileTypeOptions"
        :key="fileType"
        :value="fileType"
      >
        {{ fileType }}
      </option>
    </select>
  </section>
</template>
