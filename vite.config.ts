import { defaultExclude, defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

export default defineConfig(async () => ({
  plugins: [react(), tailwindcss()],

  test: {
    // Built in place, the Arch PKGBUILD has makepkg unpack a whole copy of
    // the repository into packaging/arch/src. Vitest's default exclude is
    // only node_modules and .git, so that copy's test files would be
    // collected next to the real ones: every test would run twice, and half
    // of the runs would be against whatever revision was last built there.
    exclude: [...defaultExclude, "packaging/**"],
  },

  resolve: {
    alias: {
      // @ts-expect-error import.meta.dirname is a nodejs global
      "@": path.resolve(import.meta.dirname, "./src"),
    },
  },

  // Vite options tailored for Tauri development, applied by `tauri dev` and
  // `tauri build`.
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
