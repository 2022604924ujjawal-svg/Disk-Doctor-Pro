import VueI18nPlugin from '@intlify/unplugin-vue-i18n/vite';
import tailwindcss from '@tailwindcss/vite';
import vue from '@vitejs/plugin-vue';
import { fileURLToPath, URL } from 'node:url';
import { defineConfig } from 'vite';

// Tauri loads development content from port 1420. Failing on a conflict
// prevents Vite from moving while the desktop window still opens the old URL.
export default defineConfig({
  build: {
    // MangoDisk supports Monterey's system WKWebView. Pinning the frontend
    // target prevents dependencies from silently raising the syntax baseline
    // to the much newer Safari version used by Vite's default target.
    target: 'safari15.6',
  },
  plugins: [
    vue(),
    VueI18nPlugin({
      // Precompile locale JSON and omit the message compiler from production.
      include: fileURLToPath(new URL('./src/locales/**', import.meta.url)),
      runtimeOnly: true,
      dropMessageCompiler: true,
      // The app uses the Composition API and needs no global i18n components.
      fullInstall: false,
    }),
    tailwindcss(),
  ],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
      '@assets': fileURLToPath(new URL('./src/assets', import.meta.url)),
    },
  },
  clearScreen: false,
  optimizeDeps: {
    // Pre-bundle dependencies used by the first interactive frame. Vite can
    // discover them lazily, but that discovery otherwise extends the visible
    // startup screen on the first Tauri development launch.
    include: ['@lucide/vue', '@vueuse/core', 'pinia', 'reka-ui', 'vue', 'vue-i18n'],
  },
  server: {
    host: '127.0.0.1',
    port: 1420,
    strictPort: true,
    // Transform the startup shell before Tauri requests it. This moves the
    // development-only cold transform waterfall ahead of the native window,
    // while production continues to use prebuilt chunks from `dist`.
    warmup: {
      clientFiles: [
        './src/main.ts',
        './src/App.vue',
        './src/assets/main.css',
        './src/layouts/**/*.vue',
        './src/pages/cleanup/**/*.vue',
        './src/components/icons/**/*.vue',
        './src/components/custom/md-empty-state.vue',
        './src/components/custom/md-operation-progress.vue',
        './src/components/custom/md-page-shell.vue',
        './src/components/custom/md-result-workspace.vue',
        './src/components/custom/md-selection-action-bar.vue',
        './src/components/ui/{button,checkbox,select}/**/*.{ts,vue}',
      ],
    },
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
});
