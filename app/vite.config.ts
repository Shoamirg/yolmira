import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri serves the built files; the dev server is only for `npm run tauri dev`.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 5173, strictPort: true, host: "127.0.0.1" },
  build: { target: "es2022", sourcemap: false },
});
