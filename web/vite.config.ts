import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";
import { mockApi } from "./mock/plugin.ts";

// `npm run dev` serves the API from the mock in ./mock.
// `CARTOGRAPH_API=http://127.0.0.1:7878 npm run dev` proxies /api to a real `cartograph serve`.
const apiTarget = process.env.CARTOGRAPH_API?.trim();

export default defineConfig({
  // Relative asset URLs, so the build works when served from `/` by the Rust binary.
  base: "./",
  plugins: [
    react(),
    ...(apiTarget
      ? []
      : [
          mockApi({
            fixture: process.env.CARTOGRAPH_FIXTURE ?? "saas",
            watchMs: Number(process.env.CARTOGRAPH_MOCK_WATCH_MS ?? 45000),
          }),
        ]),
  ],
  server: {
    port: 5173,
    proxy: apiTarget
      ? { "/api": { target: apiTarget, changeOrigin: true } }
      : undefined,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2023",
    chunkSizeWarningLimit: 700,
    // elkjs' worker script (about 1.6 MB) is copied as is, never inlined into the page.
    assetsInlineLimit: (file) => (file.includes("elk-worker") ? false : undefined),
    rolldownOptions: {
      output: {
        // Vendor chunks cache separately from the app code.
        codeSplitting: {
          groups: [
            { name: "react", test: /node_modules[\\/](react|react-dom|scheduler)[\\/]/ },
            { name: "ui-sdk", test: /node_modules[\\/]@ui-sdk[\\/]/ },
          ],
        },
      },
    },
  },
  preview: {
    port: 4173,
  },
});
