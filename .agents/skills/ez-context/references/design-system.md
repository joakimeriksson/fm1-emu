# design-system.md — text-only template & always-fresh rule

`design-system.md` captures a project's UI/visual conventions in **plain markdown** — no JSON, no TSX, no token-export files. Text is the dominant real-world convention for these docs and keeps the doc readable and agent-friendly. Create it only when a UI surface exists (see bootstrap step e). Its freshness contract is in [references/staleness.md](staleness.md): always evaluated, conditionally loaded, always re-checked on UI change.

## How to populate it

Seed from two sources, like everything else:
- **Audit:** existing design tokens, theme config, Tailwind/CSS variables, a component library, recurring patterns in existing components. Encode what's *already true* — don't invent a system that contradicts the code.
- **Interview:** the one light design question ("existing brand/style direction to encode, or propose one?"). If proposing, label proposed choices clearly as proposals, not established fact.

Write only what's non-obvious or worth standardizing. If the project has three colors and no real system yet, the doc is short and says so — don't manufacture a 40-token-scale design system for a CLI with one button.

## The 5-part skeleton

A pragmatic structure (adapt to the project; drop sections that don't apply):

1. **Design tokens** — colors (with roles: bg, surface, text, accent, semantic), typography (families, scale, weights), spacing scale, radii, shadows, breakpoints. State them as values + their *role*, in text/tables.
2. **Component architecture** — how components are organized (atomic/feature/etc.), naming conventions, where shared vs. one-off components live, composition patterns.
3. **Accessibility** — the project's a11y baseline (target e.g. WCAG 2.1 AA), focus handling, color-contrast rules, keyboard/ARIA expectations. Record the actual commitment, not aspirational boilerplate.
4. **Theming** — light/dark or brand theming approach, how themes are applied (CSS vars, context, etc.), how to add a theme-aware component.
5. **Component-doc conventions** — how components should be documented (props tables, usage examples, do/don't), so the system stays consistent as it grows.

## Frontmatter

```yaml
---
load_when: "UI work, styling, components, visual or design changes"
last_verified: 2026-06-20
source_refs:
  - src/components/**
  - src/styles/**
  - tailwind.config.*
  - "**/*.css"
---
```

Tune `source_refs` to where the project's UI and styling actually live. These globs are what the always-fresh re-check watches.

A starting template to copy: [assets/templates/design-system.md.template](../assets/templates/design-system.md.template).
