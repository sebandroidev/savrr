import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Tauri drives dev/build via beforeDevCommand/beforeBuildCommand; the fixed
// port matches tauri.conf.json `devUrl`.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    target: "es2021",
    sourcemap: false,
    outDir: "dist",
    emptyOutDir: true,
  },
});
