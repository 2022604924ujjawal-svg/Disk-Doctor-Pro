import { fileURLToPath, URL } from 'node:url';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
    coverage: {
      provider: 'v8',
      // Current coverage focuses on pure utilities, services, and state logic.
      // Include all production TypeScript so unimported files cannot inflate
      // the result. Add Vue SFCs after component mounting tests are available;
      // V8 cannot reliably parse raw SFC source that was never imported.
      include: ['src/**/*.ts'],
      exclude: ['src/**/*.test.ts', 'src/**/*.d.ts', 'src/components/ui/**', 'src/components/icons/**'],
      reporter: ['text-summary', 'html', 'lcov'],
      reportsDirectory: 'coverage',
    },
  },
});
