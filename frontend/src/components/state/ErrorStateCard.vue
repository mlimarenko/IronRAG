<script setup lang="ts">
withDefaults(
  defineProps<{
    title?: string
    message: string
    detail?: string
  }>(),
  {
    title: 'Something broke',
    detail: undefined,
  },
)
</script>

<template>
  <article
    class="error-state-card"
    role="alert"
  >
    <div
      class="error-state-card__icon"
      aria-hidden="true"
    >
      !
    </div>
    <div class="error-state-card__copy">
      <h3>{{ title }}</h3>
      <p>{{ message }}</p>
      <p
        v-if="detail"
        class="error-state-card__detail"
      >
        {{ detail }}
      </p>
    </div>
    <div
      v-if="$slots.actions"
      class="error-state-card__actions"
    >
      <slot name="actions" />
    </div>
  </article>
</template>

<style scoped>
.error-state-card {
  display: flex;
  gap: 14px;
  align-items: flex-start;
  padding: 20px;
  border: 1px solid rgb(239 68 68 / 0.25);
  border-radius: 18px;
  background: rgb(254 242 242 / 0.95);
}

.error-state-card__icon {
  display: grid;
  place-items: center;
  width: 36px;
  height: 36px;
  border-radius: 12px;
  background: rgb(239 68 68 / 0.15);
  color: #b91c1c;
  font-weight: 800;
}

.error-state-card__copy {
  display: grid;
  gap: 6px;
}

.error-state-card__copy h3,
.error-state-card__copy p {
  margin: 0;
}

.error-state-card__copy p {
  color: #7f1d1d;
}

.error-state-card__detail {
  font-size: 0.92rem;
}

.error-state-card__actions {
  margin-left: auto;
}
</style>
