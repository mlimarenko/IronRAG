<template>
  <div class="app-shell">
    <aside class="sidebar">
      <h1>RustRAG</h1>
      <nav>
        <router-link to="/"> Dashboard </router-link>
        <router-link to="/workspaces"> Workspaces </router-link>
        <router-link to="/projects"> Projects </router-link>
        <router-link to="/providers"> Providers </router-link>
        <router-link to="/ingestion"> Ingestion </router-link>
        <router-link to="/chat"> Chat </router-link>
        <router-link to="/diagnostics"> Diagnostics </router-link>
      </nav>
    </aside>
    <main class="content">
      <section class="context-banner">
        <strong>Workspace:</strong> Default workspace
        <span class="context-divider">•</span>
        <strong>Project:</strong> Active project not selected yet
      </section>
      <router-view />
    </main>
  </div>
</template>

<style scoped>
.context-banner {
  margin-bottom: 16px;
  padding: 12px 16px;
  border: 1px solid #d7dee7;
  border-radius: 12px;
  background: #f4f7fb;
  color: #314155;
}

.context-divider {
  margin: 0 8px;
  color: #7b8ba1;
}
</style>
