# ivaCAM Internationalization (i18n) — Plan & Onboarding

> **New to this work? Start here, then run `bd show <epic-id>` and `bd ready`.**
> This document is the durable reference; the bd epic + children track the work.

## Goal

Ship a **German** translation of everything user-facing in ivaCAM, with an
architecture that **extends to further languages** and whose **coverage is
tested**. Language is **auto-detected** from the desktop/browser locale, with a
**manual override** in Settings. **Project files stay language-agnostic** —
they must load/save identically regardless of the active language.

## Decisions locked in

- **Zero runtime dependencies.** In-house i18n built on Svelte 5 runes + plain
  JSON catalogs. No `svelte-i18n` / `paraglide` / `typesafe-i18n` / gettext
  crate. (Rationale: avoid npm/cargo dependency churn.)
- **Codegen for key safety.** A tiny in-repo Node script (Node built-ins only,
  no dep) turns `en.json` into a TypeScript key union, so missing/typo'd keys
  are `svelte-check` errors. This mirrors the existing
  `schema/openapi.yaml → frontend/src/lib/api/generated.ts` codegen +
  `git diff --exit-code` drift guard the repo already trusts.
- **Full scope:** Svelte UI + Rust-surfaced errors/warnings + CLI.
- **English base = plain English; German = Estlcam wording.** This honors the
  existing rule "No Estlcam terms in UI" (that rule is about the *English* UI:
  Whirling, Tapered, …). The *German* catalog uses Estlcam terms (Wirbeln,
  Kegel, …).

## Terminology sources (German / Estlcam)

- **Seed glossary:** `refs/viaconstructor/viaconstructor/locales/de/LC_MESSAGES/base.po`
  — 304 vetted domain-German entries from the Python predecessor
  (e.g. Pocket→Taschen, no Contour→keine Kontur, Zigzag→Zickzack,
  Offset→Versatz, Depth→Tiefe, Tabs→Haltestege, Lead-in/out→Start/End).
- **Estlcam wording:** lives only in `refs/Estlcam_64_13004.exe`. Extract with
  `strings` and/or by inspecting the installed Estlcam UI; reconcile the seed
  glossary to Estlcam terms in `docs/i18n/glossary-de.md`.
- **`docs/i18n/QNA_I18N.md` (terminology Q&A):** a structured list of specific
  questions / translation requests — English term + UI context + a blank
  "Estlcam wording" field — generated from `en.json` once the extraction
  sweeps land. An Estlcam user fills in the exact wording Estlcam uses, so
  `de.json` stays within well-known Estlcam terms. QNA → glossary → de.json.

## Target architecture (frontend)

```
frontend/src/lib/i18n/
  messages/
    en.json          # base — SOURCE OF TRUTH, authored by developers
    de.json          # authored by translators only
    <xx>.json        # future languages
  keys.ts            # GENERATED from en.json (do not edit)
  i18n.svelte.ts     # locale $state + t() + detect + persistence
  index.ts           # re-exports
frontend/scripts/i18n-codegen.mjs   # en.json -> keys.ts (Node built-ins only)
```

- **Key naming:** namespaced, greppable, stable —
  `settings.view.preview_style`, `ops.kind.pocket`, `dialog.unsaved.title`,
  `error.out_of_work_area`, `cli.help.import`.
- **`t(key, params?)`:** reads the `locale` `$state` (so components re-render
  on language change — no reload). Interpolation via `{name}` placeholders.
  Fallback chain: active locale → `en` → the key itself.
- **Detect + override:** on startup, if the stored setting is `auto`/unset,
  read `navigator.language` (works in Tauri webview *and* browser/wasm — no
  OS-locale plugin needed). Override via `language: 'auto' | 'en' | 'de'` on
  `AppSettings` (`frontend/src/lib/state/project.svelte`), persisted through
  the existing Tauri `plugin-store`; surfaced in `SettingsDialog.svelte`
  (Appearance tab, next to Theme).

## Target architecture (backend / CLI)

- **Errors/warnings shown in the GUI:** add a stable **`code`** enum +
  structured **`params`** to the error schema (`crates/ivac-core/src/errors.rs`);
  keep the English `message` as a fallback. The **frontend** owns the
  translated templates (`error.*` keys), rendered by `ErrorToast.svelte` from
  `code` + `params`. Keeps the schema seam clean (codes are language-agnostic
  like the op enums) and gives one translation home for everything the GUI shows.
- **CLI** (runs without the frontend): small embedded Rust catalog
  `crates/ivac-cli/i18n/{en,de}.json` via `include_str!` (no external crate).
  Locale from `LANG`/`LC_ALL`, override with `--lang`.

## Project-file compatibility

Already safe: `.vc-project.json` stores enum keys (`"kind":"pocket"`), never
labels. Phase "tests" adds a **locale-invariance regression test**: round-trip
a fixture under both locales, assert byte-identical output.

## Coverage testing (the hard requirement)

Frontend (vitest):
1. **Locale parity** — `keys(de) === keys(en)`; list missing/extra.
2. **No empty / untranslated** — no `""`; warn on `de == en` non-proper-nouns.
3. **Placeholder parity** — `{…}` tokens match between `en` and each locale.
4. **No dead keys** — every `MsgKey` is referenced in `src/`.
5. **Hardcoded-string guard** — scan `.svelte` markup for un-wrapped
   user-facing literals (the "did we miss one?" net) + allowlist.

Rust (`cargo test`):
6. CLI catalog parity (`de` keys == `en` keys).
7. Project-file locale-invariance round-trip.

CI / pre-release: add the i18n codegen drift guard + parity/coverage tests to
`.github/workflows/ci.yml` and `scripts/pre-release.sh`, beside the existing
`schema-check` / codegen-drift gates.

## Translator's workflow

- Edit **one file**: `de.json` (and future `xx.json`). Never touch components.
- `pnpm run i18n:report` lists every key missing from / identical-to English.
  "Coverage" == that report is empty.
- Follow `docs/i18n/glossary-de.md` for term consistency.
- New language = copy `en.json`→`xx.json`, add `'xx'` to the supported list +
  Settings dropdown, translate, run the report.

## How to run things

```bash
cd frontend
pnpm install
pnpm run i18n:codegen     # regenerate keys.ts from en.json (added by infra phase)
pnpm run i18n:report      # list untranslated/missing keys (added by infra phase)
pnpm exec svelte-check    # 0 errors expected (missing keys fail here)
pnpm test --run           # vitest incl. coverage tests
# repo root:
cargo test --workspace --exclude ivac-tauri   # incl. CLI parity + project invariance
```

## Phase / issue map

See the bd epic and its children. Suggested order:
1. **Infra** (locale state, `t()`, codegen, drift guard, Settings dropdown,
   detect/persist). Blocks the extraction sweeps + coverage tests.
2. **Extraction sweep A** — Settings + Machine dialogs.
3. **Extraction sweep B** — Op kinds + op-properties + app-bar/menus.
4. **Extraction sweep C** — Add-text / generate-bar / toasts / services / rest.
5. **Coverage tests + CI/pre-release gates.**
6. **Backend error codes + frontend error catalog.**
7. **CLI Rust catalog.**
8. **Glossary build + `de.json` authoring.** (Depends on the sweeps.)
9. **README_de.md + link + heading-parity check.**
10. **CONTRIBUTING recipe + docs.**
