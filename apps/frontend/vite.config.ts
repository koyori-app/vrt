import { tanstackStart } from "@tanstack/react-start/plugin/vite";
import tailwindcss from "@tailwindcss/vite";
import viteReact from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import { defineConfig, loadEnv } from "vite";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  // docker-compose publishes the backend on 127.0.0.1:3500; `cargo run` uses
  // LISTEN_ADDR (default 0.0.0.0:3400). Override with VITE_API_BASE either way.
  const apiBase = env.VITE_API_BASE ?? "http://localhost:3500";

  return {
    server: {
      port: 3000,
      proxy: {
        "/api": {
          target: apiBase,
          changeOrigin: false,
          rewrite: (path) => path.replace(/^\/api/, ""),
          // The backend CSRF middleware compares the Origin header against
          // ALLOW_ORIGIN (and against its own Host). `changeOrigin: false`
          // keeps the browser's Origin (http://localhost:3000) intact, so the
          // backend must run with ALLOW_ORIGIN=http://localhost:3000.
        },
      },
    },
    resolve: {
      alias: {
        "@": fileURLToPath(new URL("./src", import.meta.url)),
      },
    },
    plugins: [
      tailwindcss(),
      tanstackStart(),
      // react's vite plugin must come after start's vite plugin
      viteReact(),
    ],
  };
});
