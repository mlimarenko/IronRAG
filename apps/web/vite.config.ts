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
