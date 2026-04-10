import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react-swc";
import path from "path";
import { readFileSync } from "node:fs";

const packageJson = JSON.parse(
  readFileSync(path.resolve(__dirname, "package.json"), "utf8"),
) as { version?: string };

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify(packageJson.version ?? "0.0.0"),
  },
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
  },
  resolve: {
    alias: { "@": path.resolve(__dirname, "./src") },
  },
});
