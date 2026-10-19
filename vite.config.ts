import { defineConfig, loadEnv } from "vite";
// @ts-expect-error type error without @types/node package
import process from "node:process";
import { resolve } from "node:path";
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const isApprovals = process.env.VITE_FEATURE_APPROVALS === "true" ||
                      process.env.VITE_FEATURE_APPROVALS === "1" ||
                      env.VITE_FEATURE_APPROVALS === "true" ||
                      env.VITE_FEATURE_APPROVALS === "1";

  return {
    define: {
      __FEATURE_APPROVALS__: JSON.stringify(isApprovals),
      "import.meta.env.VITE_FEATURE_APPROVALS": JSON.stringify(isApprovals ? "true" : "false"),
    },
    // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
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
    build: {
      rollupOptions: {
        input: {
          main: resolve(process.cwd(), "index.html"),
          menu: resolve(process.cwd(), "menu.html"),
          settings: resolve(process.cwd(), "settings.html"),
        },
      },
    },
  };
});
