<script setup lang="ts">
import { useI18n } from 'vue-i18n'

const { t } = useI18n()
</script>

<template>
  <div class="rr-not-found">
    <div class="rr-not-found__card">
      <h1>404</h1>
      <p>{{ t('notFound.message', 'Page not found') }}</p>
      <router-link to="/" class="rr-button rr-button--primary">
        {{ t('notFound.home', 'Go to home') }}
      </router-link>
    </div>
  </div>
</template>

<style scoped lang="scss">
.rr-not-found {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: calc(100svh - 5.5rem);
  padding: 1.5rem;
}

.rr-not-found__card {
  display: grid;
  gap: 1rem;
  width: min(100%, 26rem);
  padding: 1.8rem 1.5rem;
  border: 1px solid rgba(226, 232, 240, 0.85);
  border-radius: 22px;
  background:
    radial-gradient(circle at top left, rgba(99, 102, 241, 0.08), transparent 36%),
    linear-gradient(180deg, rgba(255, 255, 255, 0.98), rgba(248, 250, 252, 0.96));
  box-shadow: 0 18px 40px rgba(15, 23, 42, 0.06);
  text-align: center;
}

.rr-not-found h1 {
  margin: 0;
  font-size: 5rem;
  line-height: 1;
  letter-spacing: -0.04em;
  color: rgba(15, 23, 42, 0.18);
}

.rr-not-found p {
  margin: 0;
  font-size: 1.15rem;
  color: rgba(15, 23, 42, 0.56);
}

@media (max-width: 640px) {
  .rr-not-found {
    min-height: calc(100svh - 4.6rem);
    padding: 1rem;
  }

  .rr-not-found__card {
    padding: 1.45rem 1.1rem;
    border-radius: 18px;
  }

  .rr-not-found h1 {
    font-size: 4.2rem;
  }

  .rr-not-found p {
    font-size: 1rem;
  }
}
</style>
