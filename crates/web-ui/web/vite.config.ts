import { defineConfig } from "vite";
import tailwindcss from "@tailwindcss/vite";

// During `vite dev` the Rust server runs separately (default port 4137);
// proxy the WebSocket and the out-of-band upload/download routes to it so
// the frontend gets HMR while talking to the real agent backend.
export default defineConfig({
  plugins: [tailwindcss()],
  server: {
    proxy: {
      "/ws": { target: "ws://127.0.0.1:4137", ws: true },
      "/upload": "http://127.0.0.1:4137",
      "/download": "http://127.0.0.1:4137",
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
