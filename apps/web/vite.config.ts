import { readFileSync } from "node:fs";
import path from "node:path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react-swc";
import tailwindcss from "@tailwindcss/vite";

const packageJson = JSON.parse(
  readFileSync(path.resolve(__dirname, "package.json"), "utf8"),
) as { version?: string };

const normalizeBuildVersion = (value?: string) => {
  const trimmed = value?.trim();
  if (!trimmed) {
    return packageJson.version ?? "0.0.0";
  }
  return trimmed.replace(/^v(?=\d)/, "");
};

const appVersion = normalizeBuildVersion(
  process.env.APP_VERSION ?? process.env.VITE_APP_VERSION,
);

const vendorChunk = (id: string) => {
  const normalizedId = id.replaceAll("\\", "/");
  if (!normalizedId.includes("/node_modules/")) {
    return null;
  }

  if (
    normalizedId.includes("/node_modules/swagger-ui-react/") ||
    normalizedId.includes("/node_modules/swagger-client/") ||
    normalizedId.includes("/node_modules/swagger-ui/")
  ) {
    return "vendor-swagger";
  }

  if (
    normalizedId.includes("/node_modules/@tiptap/") ||
    normalizedId.includes("/node_modules/@prosemirror/") ||
    normalizedId.includes("/node_modules/prosemirror-") ||
    normalizedId.includes("/node_modules/orderedmap/")
  ) {
    return "vendor-tiptap";
  }

  if (
    normalizedId.includes("/node_modules/@sigma/") ||
    normalizedId.includes("/node_modules/sigma/") ||
    normalizedId.includes("/node_modules/graphology")
  ) {
    return "vendor-sigma";
  }

  if (
    normalizedId.includes("/node_modules/react/") ||
    normalizedId.includes("/node_modules/react-dom/") ||
    normalizedId.includes("/node_modules/react-router") ||
    normalizedId.includes("/node_modules/scheduler/")
  ) {
    return "vendor-react";
  }

  if (normalizedId.includes("/node_modules/@tanstack/")) {
    return "vendor-query";
  }

  if (
    normalizedId.includes("/node_modules/i18next/") ||
    normalizedId.includes("/node_modules/react-i18next/")
  ) {
    return "vendor-i18n";
  }

  if (normalizedId.includes("/node_modules/@opentelemetry/")) {
    return "vendor-observability";
  }

  return null;
};

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify(appVersion),
  },
  server: {
    host: "::",
    port: 3000,
    proxy: {
      "/v1": {
        target: "http://127.0.0.1:19000",
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
    sourcemap: true,
    rolldownOptions: {
      output: {
        codeSplitting: {
          includeDependenciesRecursively: true,
          groups: [
            {
              name: vendorChunk,
              test: /node_modules/,
            },
          ],
        },
      },
    },
  },
  plugins: [tailwindcss(), react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
    dedupe: [
      "react",
      "react-dom",
      "react/jsx-runtime",
      "react/jsx-dev-runtime",
      "@tanstack/react-query",
      "@tanstack/query-core",
    ],
  },
});
