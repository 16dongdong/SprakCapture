import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

/**
 * 将配置文件相对路径转换为 Vite 可识别的绝对路径。
 * Windows 文件 URL 带有额外的首个斜杠，必须在解析别名前移除。
 */
function resolveProjectPath(relativePath: string): string {
  return decodeURIComponent(new URL(relativePath, import.meta.url).pathname).replace(
    /^\/(?=[A-Za-z]:\/)/,
    "",
  );
}

const sourceDirectory = resolveProjectPath("./src");
const testSupportDirectory = resolveProjectPath("./tests/support");

/**
 * 精确识别 React 运行时包，避免将 `@radix-ui/react-*` 一并拆入运行时块。
 *
 * Vite 的依赖路径会在 pnpm 下多出 `.pnpm/<包>/node_modules` 片段；该表达式同时覆盖扁平与 pnpm 嵌套路径。
 * 错误语义：不匹配的第三方包统一留在 vendor 块，保证依赖图没有反向循环。
 */
const reactRuntimeModulePattern =
  /\/node_modules\/(?:\.pnpm\/[^/]+\/node_modules\/)?(?:react|react-dom|scheduler|use-sync-external-store)\//;
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": sourceDirectory,
      "#tests": testSupportDirectory,
    },
  },
  server: {
    // 开发态默认允许局域网直接查看 Web，认证门禁仅在生产远程入口启用。
    host: "0.0.0.0",
    port: 5173,
    // Tauri devUrl 与控制面 Origin 白名单共同依赖固定端口，端口占用必须直接失败。
    strictPort: true,
  },
  build: {
    rollupOptions: {
      output: {
        /**
         * 将稳定第三方运行库与业务代码分离，避免功能增长使单入口块超过浏览器缓存粒度。
         */
        manualChunks(moduleId) {
          if (moduleId.indexOf("node_modules") === -1) {
            return undefined;
          }
          if (reactRuntimeModulePattern.test(moduleId)) {
            return "reactRuntime";
          }
          if (moduleId.indexOf("i18next") !== -1) {
            return "localization";
          }
          if (moduleId.indexOf("lucide") !== -1) {
            return "icons";
          }
          return "vendor";
        },
      },
    },
  },
  test: {
    environment: "jsdom",
    include: ["tests/unit/**/*.test.{ts,tsx}"],
    setupFiles: "./tests/support/testSetup.ts",
    css: true,
  },
});
