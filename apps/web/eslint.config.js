import js from "@eslint/js";
import globals from "globals";
import pluginQuery from "@tanstack/eslint-plugin-query";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import tseslint from "typescript-eslint";

const featureNames = [
  "admin",
  "assistant",
  "auth",
  "dashboard",
  "documents",
  "graph",
  "swagger",
];

// Feature Boundary Imports
// app owns routing and may import features. features may import their own
// feature, shared modules, and app shell modules. shared must stay independent
// of feature modules.
const featureBoundaryConfig = (featureName) => {
  const foreignFeaturePattern = featureNames
    .filter((name) => name !== featureName)
    .join("|");

  return {
    files: [`src/features/${featureName}/**/*.{ts,tsx}`],
    rules: {
      "no-restricted-imports": [
        "error",
        {
          patterns: [
            {
              regex: `^@/features/(?!${featureName}(?:/|$))`,
              message:
                "Feature modules may only import their own feature, shared modules, or app modules.",
            },
            {
              regex: `(?:^|/)features/(?!${featureName}(?:/|$))`,
              message:
                "Feature modules may only import their own feature, shared modules, or app modules.",
            },
            {
              regex: `^\\.\\./(?:\\.\\./)*(?:${foreignFeaturePattern})(?:/|$)`,
              message: "Cross-feature relative imports are forbidden.",
            },
            {
              regex: `(?:^|/)\\.\\./(?:${foreignFeaturePattern})(?:/|$)`,
              message: "Cross-feature traversal imports are forbidden.",
            },
          ],
        },
      ],
    },
  };
};

export default tseslint.config(
  {
    ignores: [
      "coverage",
      "dist",
      "storybook-static",
      "public/mockServiceWorker.js",
      "src/shared/api/generated/**",
    ],
  },
  {
    extends: [
      js.configs.recommended,
      ...tseslint.configs.recommendedTypeChecked,
      ...pluginQuery.configs["flat/recommended"],
    ],
    files: ["**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: 2020,
      globals: globals.browser,
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": ["warn", { allowConstantExport: true }],
      "@typescript-eslint/no-unused-vars": "off",
      "@typescript-eslint/no-floating-promises": "error",
      "@typescript-eslint/no-misused-promises": [
        "error",
        { checksVoidReturn: { attributes: false } },
      ],
      "@typescript-eslint/consistent-type-imports": [
        "error",
        { prefer: "type-imports", fixStyle: "separate-type-imports" },
      ],
      "@typescript-eslint/only-throw-error": "error",
      "@typescript-eslint/no-unnecessary-type-assertion": "error",
      "@typescript-eslint/no-explicit-any": "error",
      "@typescript-eslint/no-base-to-string": "off",
      "@typescript-eslint/no-duplicate-type-constituents": "off",
      "@typescript-eslint/no-redundant-type-constituents": "off",
      "@typescript-eslint/no-unsafe-argument": "off",
      "@typescript-eslint/no-unsafe-assignment": "off",
      "@typescript-eslint/no-unsafe-call": "off",
      "@typescript-eslint/no-unsafe-member-access": "off",
      "@typescript-eslint/no-unsafe-return": "off",
      "@typescript-eslint/require-await": "off",
      "@typescript-eslint/restrict-template-expressions": "off",
      "@typescript-eslint/unbound-method": "off",
      "react-hooks/set-state-in-effect": "error",
      "react-hooks/refs": "error",
      "react-hooks/preserve-manual-memoization": "error",
      "react-hooks/purity": "error",
      "react-hooks/immutability": "error",
      "react-hooks/static-components": "error",
      "@tanstack/query/no-unstable-deps": "error",
      // Sprint 2d: server-state reads must flow through TanStack Query
      // (`useQuery(queries.*Options(...))`), never through ad-hoc
      // useEffect + fetch / *Api loops. The selectors below block the two
      // canonical anti-patterns:
      //   1. `useEffect(() => { fetch(...) })` — the literal browser fetch
      //      call inside any function nested under useEffect.
      //   2. `useEffect(() => { fooApi.bar() })` — calling any imperative
      //      `*Api.*` shim inside useEffect. Mutations belong in event
      //      handlers / useMutation; reads belong in useQuery.
      // Tests and the generated SDK layer are exempt because they describe
      // expectations rather than running production code.
      "no-restricted-syntax": [
        "error",
        {
          selector:
            "CallExpression[callee.name='useEffect'] :function CallExpression[callee.name='fetch']",
          message:
            "useEffect+fetch is forbidden. Use useQuery(queries.*Options(...)) for server-state reads.",
        },
        {
          selector:
            "CallExpression[callee.name='useEffect'] :function CallExpression[callee.object.name=/.+Api$/]",
          message:
            "Calling `*Api.method()` from inside useEffect is forbidden. Reads → useQuery(queries.*Options); mutations → useMutation or event handlers.",
        },
      ],
    },
  },
  ...featureNames.map(featureBoundaryConfig),
  {
    files: [
      "*.config.ts",
      ".storybook/**/*.ts",
      "playwright-fixture.ts",
      "tests/**/*.ts",
      "visual-qa/**/*.ts",
    ],
    extends: [tseslint.configs.disableTypeChecked],
  },
  {
    files: ["**/*.test.{ts,tsx}", "tests/e2e/**", "src/test/**"],
    rules: {
      "@typescript-eslint/no-explicit-any": "off",
    },
  },
  {
    files: ["src/shared/**/*.{ts,tsx}"],
    rules: {
      "no-restricted-imports": [
        "error",
        {
          patterns: [
            {
              regex: "^@/features/",
              message: "Shared modules must not import feature modules.",
            },
            {
              regex: "(?:^|/)features/",
              message: "Shared modules must not import feature modules.",
            },
            {
              regex: "^\\.\\./(?:\\.\\./)*features/",
              message: "Shared modules must not import feature modules.",
            },
          ],
        },
      ],
    },
  },
  {
    // Tests can still drive the imperative shims directly when validating
    // contract behaviour — the rule above only covers production code.
    files: [
      "**/*.test.ts",
      "**/*.test.tsx",
      "**/*.stories.{ts,tsx}",
      "**/test/**",
      "**/__tests__/**",
    ],
    rules: {
      "no-restricted-syntax": "off",
    },
  },
);
