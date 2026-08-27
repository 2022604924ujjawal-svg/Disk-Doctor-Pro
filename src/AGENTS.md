# Frontend Guidelines

This file applies only to `src/` and inherits the repository-wide rules in [`../AGENTS.md`](../AGENTS.md).

## Boundaries and layout

- `pages/<domain>/` contains route-level views and components used only by that page.
- `layouts/` contains the application shell and cross-page layout components; layouts are not pages.
- `components/custom/` contains project-owned reusable UI primitives.
- `components/icons/` contains project-owned icon components.
- `components/ui/` is generated Shadcn-Vue code. Do not edit it for project-wide behavior; configure it or wrap it in `components/custom/`.
- `stores/` owns UI and workflow state for one domain. A Store may coordinate services but must not copy another Store's complete state.
- `lib/services/` owns side effects: Tauri invocation, persistence, platform integration, browser APIs, dialogs, and event subscriptions.
- `lib/utils/` owns deterministic functions only. Utilities must not read Stores, invoke Tauri, access storage, or mutate external state.
- `lib/models/` contains frontend-owned protocols and constants split by concrete domain. Business code imports the owning file directly; narrow generated-component adapters are not public APIs.

Pages may present several domains together, but shared product orchestration must remain explicit; do not create a frontend `manager` or a global Store containing every workflow.

## Vue and TypeScript

- Use Vue 3 `<script setup lang="ts">` and strict TypeScript. Do not introduce `any`.
- Project-owned Vue files use `kebab-case`; reusable custom components and icon files use the `md-` prefix so their ownership is recognizable as MangoDisk code.
- Prefer props and emits for component communication. Do not use provide/inject as a hidden event bus.
- Pinia stores use the Options API (`state`, `getters`, `actions`).
- Prefer exported module functions or static service methods for stateless adapters. Use an owned service instance when it has lifecycle state, replaceable dependencies, or requires test isolation.
- Avoid new composables and generic `use*` helpers when a named Store, static service, or pure utility gives clearer IDE navigation.
- Import business code from its concrete file. The minimal indexes required by generated Shadcn code are not public project APIs.
- Do not duplicate Rust protocol types by guessing. When protocol bindings are maintained manually, update Rust, TypeScript, services, Stores, and compatibility tests in one change.

## Text, status, and logging

- All user-facing strings belong in locale resources. Update every supported locale in the same change.
- Constants are domain-owned. Do not move every unrelated constant into a new global constants file.
- Render behavior from typed status, risk, capability, and reason codes. Free-form backend messages are diagnostics, not UI control flow.
- Use the project logger service for meaningful lifecycle, failure, and recovery events. Do not use raw `console.*` in production paths.
- Frontend logs must not contain raw filesystem paths, file contents, installation identifiers, or unrelated user-specific metadata. File names may be logged when they are materially useful for diagnosis, but keep them separate from parent paths and avoid broader private metadata. Prefer typed events, counts, timings, operation IDs, and redacted diagnostics.
- Do not localize rule resources. Resolve stable rule IDs and diagnostic codes at the presentation boundary.

## Styling and interaction

- The project uses Tailwind CSS 4.3. Verify syntax against v4 documentation.
- Prefer responsive utilities and named container queries over page-specific viewport media queries. `shell:` is reserved for application-shell navigation.
- Reusable components fill or shrink within their parent. Exact sizes are appropriate for stable control heights, icon boxes, hit targets, and intrinsic-ratio assets—not page layout widths.
- Put cross-page scrollbar behavior in `assets/main.css`; use `scrollbar-hidden` or `scrollbar-stable` in components.
- In scoped CSS, add `@reference "@assets/main.css"` before `@apply`.
- Buttons and hover states must not translate, scale, or change layout dimensions. Use color, border, or shadow feedback that cannot cause page movement.
- Keep page headers and content-height behavior consistent through project-owned shell components.
- Never place raw SVG markup in a template. Add or reuse a component under `components/icons/`.

## Validation

For frontend changes, run:

```sh
pnpm check:frontend
```

Also run `pnpm check` when a change touches Rust-facing protocols, generated bindings, events, configuration, or packaging. Test affected UI flows in both themes and all supported locales when text or layout changes, and verify the minimum window size for responsive changes.
