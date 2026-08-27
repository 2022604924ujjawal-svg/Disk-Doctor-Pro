import eslint from '@eslint/js';
import eslintConfigPrettier from 'eslint-config-prettier';
import pluginVue from 'eslint-plugin-vue';
import globals from 'globals';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  {
    ignores: [
      'dist/**',
      'node_modules/**',
      'src/components/ui/**',
      'src-tauri/**',
    ],
  },
  eslint.configs.recommended,
  ...tseslint.configs.recommended,
  ...pluginVue.configs['flat/recommended'],
  {
    files: ['src/**/*.{ts,vue}'],
    languageOptions: {
      globals: globals.browser,
      parserOptions: {
        parser: tseslint.parser,
      },
    },
    rules: {
      // Vue convention keeps App.vue as the root component. Project-owned
      // components continue to use multi-word wx-* names.
      'vue/multi-word-component-names': ['error', { ignores: ['App', 'index'] }],
      'vue/no-undef-properties': 'error',
      'no-restricted-imports': [
        'error',
        {
          paths: [
            {
              name: '@/lib/models',
              message: 'Import the concrete domain model instead of the generated-component adapter.',
            },
            {
              name: '@/lib/utils',
              message: 'Import the concrete utility instead of the generated-component adapter.',
            },
            {
              name: '@/components/icons',
              message: 'Import the concrete icon component instead of the generated-component adapter.',
            },
          ],
        },
      ],
      'no-console': 'error',
    },
  },
  {
    files: ['src/lib/services/logger-service.ts'],
    rules: {
      'no-console': 'off',
    },
  },
  {
    files: ['scripts/**/*.mjs'],
    languageOptions: {
      globals: globals.node,
    },
  },
  eslintConfigPrettier,
);
