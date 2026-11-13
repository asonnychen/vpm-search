import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    host: true,
    port: 5173,
    strictPort: true,
  },
  test: {
    // 只跑前端单测；tests/*.test.mjs 是阶段 1 的 node:test 契约测试。
    include: ["src/**/*.test.ts"],
    environment: "node",
  },
});
