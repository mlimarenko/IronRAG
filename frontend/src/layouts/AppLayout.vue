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
      </nav>
    </aside>
    <main class="content">
      <router-view />
    </main>
  </div>
</template>
