---
load_when: "UI work, styling, components, visual or design changes"
last_verified: 2026-11-16
source_refs:
  - context/icons.md
  - assets/icons/README.md
  - tools/generate-icons.swift
  - web/studio/index.html
  - web/studio/studio.css
  - web/studio/app.js
  - web/main.js
  - crates/fm1-live/src/panel.rs
  - crates/fm1-plugin/src/editor.rs
  - crates/fm1-live/src/main.rs
  - web/hardware/index.html
---
# Design system

Status: proposed visual direction, implemented in the browser Studio. The base-8 requirement is user-confirmed. Colors, typography and styling are an initial design proposal. This document describes new UI; it does not assert that the existing native or WASM hardware panel follows these tokens.

## Principles

Make the instrument the main event. Use quiet surfaces and strong hierarchy; reserve saturated color for actions and meaningful state. Keep the software/hardware destination and actual capability clear. Never decorate with invented meters, CPU values, audio waveforms or connected-device indicators.

Space is functional: controls should be comfortable while leaving room for the device. Musical state and firmware data outrank explanatory engineering detail in the main flow.

## Tokens and measurements

### Base-8 geometry

Use 8, 16, 24, 32, 40, 48 and 64 px for spacing. New layout sizes and control geometry are multiples of 8. Supported exceptions are 1 px borders, 2 px focus outlines, fractional responsive dimensions and typographic sizes/letter spacing. Existing emulated hardware artwork keeps its measured proportions.

| Role | Value |
|---|---|
| Standard control height | 48 px |
| Compact control height | 40 px |
| Main desktop inset | 48 px |
| Main tablet inset | 32 / 24 px |
| Main mobile horizontal inset | 16 px |
| Card/grid gap | 24 px; 16 px when space is constrained |
| Sidebar width | 248 px; 200 px compact |
| Topbar minimum height | 96 px |
| Control/card radius | 8 px |
| Instrument well radius | 16 px |
| Focus ring | 2 px with 4 px offset; deliberate optical/accessibility exception |

CSS custom properties live in studio.css. Feature styles consume semantic color/spacing roles. Arbitrary inline spacing is forbidden. The design specimen uses inline widths/colors only to demonstrate the published scale.

### Color

| Token | Value | Role |
|---|---|---|
| --paper | #f3f1ea | Application background |
| --surface | #faf9f5 | Cards, sidebar and secondary controls |
| --ink | #222623 | Primary text and selected navigation |
| --muted | #62675f | Supporting text; never opacity-reduced for readable content |
| --line | #d5d8cf | Structural dividers, not sole interactive affordance |
| --accent | #2c4cd4 | Primary actions, links and focus |
| --accent-hover | #203baa | Hovered primary action |
| --signal | #b8462b | Small section identifiers; error-family cue |
| --success | #35704b | Available/offline indicators |

State is always named in text, not conveyed solely by a colored dot. A ready-offline indicator requires successful local asset caching; network connectivity alone does not earn it. Hardware connection state is currently explicitly unavailable.

### Type

Use platform system sans for prose/controls and platform monospace for hashes, technical values and section identifiers. No downloaded fonts.

| Role | Size / line height |
|---|---|
| Display, desktop | 48 / 56 px; 56 / 64 on wide displays |
| Display, compact | 32 / 40 px |
| Section heading | 24 / 32 px |
| Body / card heading | 16 / 24 px |
| Supporting body / controls | 14 / 24 px |
| Label / status | 12 / 16 px |
| Technical micro-label | 10 / 16 px; optional, not critical instructions |

Weights: 400 body, 500 headings/actions, 600 compact card titles. Display tracking is an optical exception to the grid. Avoid extensive all-caps prose; uppercase monospace is limited to short navigational labels.

## Components and states

The current code uses native HTML and small DOM-building functions rather than a framework. Shared primitives are button, nav-button, info-card, library-card, tag, status-label and notice. Do not introduce a framework just for one feature.

Primary buttons trigger the next meaningful action. Secondary buttons support alternate workflows. Links/quiet buttons handle export and supporting navigation. Minimum interactive height is 40 px; use 48 px for primary actions and import controls.

Every async operation needs idle, busy, success and actionable error behavior. Disable duplicate submission while an operation is in flight. Preserve the previous collection on failed validation or storage writes. Empty states explain what can be imported and offer a working action; they do not show invented user content.

Library cards display the actual imported name, package size, immutable hash and local availability. Device actions remain inside the real emulated panel. Unsupported editors and transfers must not look like functional controls.

## Accessibility

Target WCAG AA for new UI. Native semantic buttons, labels and inputs are the baseline. All meaningful controls must work by keyboard, with a visible focus ring. The skip link targets the main workspace. Navigation uses aria-current; selected firmware uses aria-pressed. Errors/status use live regions without announcing high-frequency audio data.

Check contrast when adding tokens: 4.5:1 for ordinary text and 3:1 for large text/UI boundaries where required. Low-contrast divider lines are decorative only. Do not rely on a screenshot or token name as proof of compliance.

The upstream canvas device panel has accessibility limitations; it needs its own future accessible-control layer. A labeled iframe does not make the controls inside the canvas accessible. Do not claim end-to-end accessibility until that is addressed.

Respect reduced-motion. Current UI has no essential animation. Long hashes wrap and names must not break cards. Test keyboard flow, browser zoom, narrow screens and storage errors.

## Responsive and theme behavior

Desktop: persistent collection rail, instrument with supporting cards. At 1120 px compact insets/navigation; at 800 px single-column cards and reduced display scale; at 560 px the collection becomes a horizontal/top region. Breakpoints are themselves divisible by 8.

Only the documented light Studio theme is implemented. The emulated panel retains its own theme. Future themes use semantic variables and equivalent state contrast. Native new UI should translate this scale into logical points without changing the firmware framebuffer.

## Documentation contract

For each new reusable control, record its purpose, available states, keyboard behavior, destructive effects, token use and any exceptions here or in a focused component note. Keep implementation and this document synchronized on every UI change. The in-app Design system page is a visual specimen; this Markdown document is the design contract.

## Controller and encoder interaction increment

The device page adds a labeled Controller select with Auto / Raw states, native keyboard selection and a 40 px minimum height, 8 px inset/radius. It changes host routing only. MiniLab page mapping follows the physical panel; pads retain note performance and never silently invoke Save/Record. Hardware canvas encoders use an 8 px drag detent, Shift for quarter sensitivity, and fractional visual positions with immediate discrete firmware input. Visual motion is decorative; it never reports finer firmware values. Native polling is up to 120 Hz, browser drawing follows requestAnimationFrame. Existing hardware artwork remains the documented geometry exception.

## Hardware Library

The companion is a separate, plainly labeled hardware destination. Its light neutral page uses base-8 spacing, 48 px minimum actions, native labeled port selectors and a status/progress region. Three sequential sections cover connection, project download and reviewed restore. Write controls stay disabled until identity, project selection and safety-backup acknowledgement permit them. No hardware connection status is inferred from an emulator. The browser and native embedded page share identical markup/styles.

Native macOS menu bar uses File / Audio / Instrument / Hardware / Help, grouped by task. Open uses an ellipsis because it requests a file; standard Cmd+O/Cmd+W shortcuts are retained. No speculative Save/Undo actions are presented for firmware-owned state. Native menu actions reuse existing application handlers; no hardware writes occur on menu launch. Reference: https://developer.apple.com/design/human-interface-guidelines/menus

## App identity and icons

The user-approved icon uses the actual FM-1 control arrangement, no text, a blank unlit screen, ivory markers and smooth flat charcoal materials. Body fill is uniformly #292a2b; control faces and trays must also have no grain, mottling or metallic texture. Gentle top light and local contact shadows provide depth. Modern macOS artwork has no baked outer bevel or padding; the platform supplies the enclosure. Other platforms receive their required silhouette or masking variants. The full intent, asset distinctions and per-platform commands are maintained in [icons.md](icons.md); read it before changing artwork or packaging.
