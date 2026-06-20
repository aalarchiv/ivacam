<script lang="ts">
  /// About dialog: build identity + license + third-party
  /// attributions. Lazy-loaded from App.svelte's Help → About menu
  /// item; replaces the old "Check for updates" entry now that the
  /// auto-updater is gone.
  ///
  /// Build version comes from the `__IVAC_BUILD_VERSION__` define
  /// baked by vite.config.ts (git describe --always --dirty at
  /// compile time).
  import Modal from './Modal.svelte';
  import aboutMd from 'virtual:about';
  import { renderMarkdown } from './markdown-lite';
  import { onExternalLinkClick } from '../services/external-links';

  interface Props {
    onClose: () => void;
    /// Render inline (Help tab column) instead of as a modal.
    embedded?: boolean;
  }
  let { onClose, embedded = false }: Props = $props();

  /// Prose (tagline, license, acknowledgements) authored in
  /// docs/ABOUT.md and substituted + inlined at build time by the
  /// `ivac-about-md` Vite plugin (`virtual:about`). Trusted, compiled-in
  /// content — safe for `{@html}`; markdown-lite escapes raw HTML first.
  const aboutHtml = renderMarkdown(aboutMd);

  const buildVersion =
    typeof __IVAC_BUILD_VERSION__ === 'string' ? __IVAC_BUILD_VERSION__ : 'unknown';
  /// ISO-8601 UTC timestamp baked at vite-build time. Rendered in the
  /// user's locale below the build identifier so bug reports carry
  /// "this is the build I produced on date X" alongside the commit.
  const buildDateIso = typeof __IVAC_BUILD_DATE__ === 'string' ? __IVAC_BUILD_DATE__ : '';
  const buildDateDisplay = (() => {
    if (!buildDateIso) return '';
    const d = new Date(buildDateIso);
    if (isNaN(d.getTime())) return buildDateIso;
    // Date + time in the user's locale + UTC ISO suffix so it stays
    // unambiguous when pasted into issues across timezones.
    return `${d.toLocaleString()} (${buildDateIso})`;
  })();

  let copyState = $state<'idle' | 'copied'>('idle');
  async function copyBuildId() {
    try {
      await navigator.clipboard.writeText(buildVersion);
      copyState = 'copied';
      setTimeout(() => {
        copyState = 'idle';
      }, 1500);
    } catch {
      // Clipboard API unavailable (insecure context, permissions).
      // The text is also `user-select: text` so the user can copy by
      // hand - silent fallback.
    }
  }

  /// Third-party attribution list. Hand-curated rather than auto-derived
  /// from Cargo.lock / package.json so we can group by role and link to
  /// each project's home page. Update when a dep is added or removed.
  /// All listed crates ship in at least one transport (desktop, server,
  /// or WASM bundle).
  const thirdParty: ReadonlyArray<{
    name: string;
    license: string;
    role: string;
    home: string;
  }> = [
    {
      name: 'cavalier_contours',
      license: 'MIT/Apache-2.0',
      role: 'parallel offsets, polyline boolean ops',
      home: 'https://github.com/jbuckmccready/cavalier_contours',
    },
    {
      name: 'clipper2-rust',
      license: 'BSL-1.0',
      role: 'pocket cascade + region booleans (pure-Rust Clipper2 port)',
      home: 'https://github.com/larsbrubaker/clipper2-rust',
    },
    {
      name: 'voronator',
      license: 'MIT',
      role: 'Delaunay triangulation for V-Carve medial axis',
      home: 'https://github.com/fesoliveira014/voronator-rs',
    },
    {
      name: 'dxf',
      license: 'MIT',
      role: 'DXF file reader',
      home: 'https://github.com/IxMilia/dxf-rs',
    },
    {
      name: 'usvg',
      license: 'MIT/Apache-2.0',
      role: 'SVG parsing (vector import)',
      home: 'https://github.com/linebender/resvg',
    },
    {
      name: 'ttf-parser',
      license: 'MIT/Apache-2.0',
      role: 'TTF/OTF font parsing (text-to-paths)',
      home: 'https://github.com/harfbuzz/ttf-parser',
    },
    {
      name: 'serde + serde_json',
      license: 'MIT/Apache-2.0',
      role: 'project file serialisation',
      home: 'https://serde.rs/',
    },
    {
      name: 'schemars',
      license: 'MIT',
      role: 'JSON Schema generation for the wire protocol',
      home: 'https://graham.cool/schemars/',
    },
    {
      name: 'wasm-bindgen',
      license: 'MIT/Apache-2.0',
      role: 'Rust ↔ WASM bridge',
      home: 'https://rustwasm.github.io/wasm-bindgen/',
    },
    {
      name: 'tauri',
      license: 'MIT/Apache-2.0',
      role: 'desktop shell (window, FS, IPC)',
      home: 'https://tauri.app/',
    },
    {
      name: 'Svelte',
      license: 'MIT',
      role: 'frontend reactive UI framework',
      home: 'https://svelte.dev/',
    },
    {
      name: 'three.js',
      license: 'MIT',
      role: '3D preview + heightfield rendering',
      home: 'https://threejs.org/',
    },
    {
      name: 'Vite',
      license: 'MIT',
      role: 'frontend dev server + production bundler',
      home: 'https://vitejs.dev/',
    },
  ];
</script>

{#snippet shell()}
  <header>
    <h2 id="about-title">About ivaCAM</h2>
    <div class="header-right">
      <img class="about-logo" src="/ivacam-logo.png" alt="ivaCAM logo" width="80" height="80" />
      {#if !embedded}
        <button type="button" class="dlg-close" onclick={onClose} aria-label="Close">×</button>
      {/if}
    </div>
  </header>
  <!-- Click delegate routes https links to the system browser — the
       embedded webview blocks external navigation, so plain anchors
       would silently do nothing under Tauri. -->
  <section class="about-md" role="presentation" onclick={onExternalLinkClick}>
    <!-- eslint-disable-next-line svelte/no-at-html-tags -- trusted, compiled-in ABOUT.md; markdown-lite escapes raw HTML and whitelists link hrefs -->
    {@html aboutHtml}
  </section>

  <section>
    <dl class="build">
      <dt>Build</dt>
      <dd>
        <code class="build-id">{buildVersion}</code>
        <button
          type="button"
          class="copy-btn"
          onclick={copyBuildId}
          title="Copy the build identifier so it can go in a bug report"
        >
          {copyState === 'copied' ? 'Copied' : 'Copy'}
        </button>
      </dd>
      {#if buildDateDisplay}
        <dt>Date</dt>
        <dd>
          <span
            class="build-date"
            title="UTC timestamp the binary was produced at — for cross-timezone bug reports."
          >
            {buildDateDisplay}
          </span>
        </dd>
      {/if}
    </dl>
    <p class="hint">
      Include the build identifier above when filing issues - it pins the report to the exact binary
      you tested.
    </p>
  </section>

  <section>
    <h3>Third-party libraries</h3>
    <p class="hint">
      ivaCAM stands on the work of many open-source projects. The major runtime dependencies are
      listed below; full transitive lists ship in <code>Cargo.lock</code>
      and <code>package-lock.json</code>.
    </p>
    <table class="libs">
      <thead>
        <tr>
          <th>Library</th>
          <th>License</th>
          <th>Role</th>
        </tr>
      </thead>
      <tbody>
        {#each [...thirdParty].sort( (a, b) => a.name.localeCompare( b.name, undefined, { sensitivity: 'base' }, ), ) as lib (lib.name)}
          <tr>
            <td>
              <a href={lib.home} target="_blank" rel="noreferrer">{lib.name}</a>
            </td>
            <td>{lib.license}</td>
            <td>{lib.role}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </section>
{/snippet}

{#if embedded}
  <section class="embedded-col">{@render shell()}</section>
{:else}
  <Modal
    {onClose}
    modalClass="about-modal"
    persistKey="about"
    width="min(640px, 95vw)"
    draggable
    resizable
    ariaLabelledBy="about-title"
  >
    {@render shell()}
  </Modal>
{/if}

<style>
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 1rem 1.25rem 0.5rem 1.25rem;
    border-bottom: 1px solid var(--border);
    /* Opaque background so scrolled content doesn't bleed through the
       sticky header in this draggable (floating) modal — matches the
       Tool-library / Settings dialog headers. */
    background: var(--bg-elevated);
  }
  header h2 {
    margin: 0;
    font-size: 1.05rem;
  }
  .header-right {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  /* Prominent ivaCAM logo, top-right of the About screen. */
  .about-logo {
    width: 80px;
    height: 80px;
    object-fit: contain;
    flex: 0 0 auto;
  }
  section {
    padding: 0.75rem 1.25rem;
    border-bottom: 1px solid var(--border);
  }
  section:last-of-type {
    border-bottom: 0;
  }
  section h3 {
    margin: 0 0 0.4rem 0;
    font-size: 0.9rem;
    color: var(--text-strong);
  }
  /* The About prose is injected via {@html} from ABOUT.md, so its
     elements aren't Svelte-scoped — target them with :global under the
     scoped .about-md wrapper. */
  .about-md :global(h3) {
    margin: 0.9rem 0 0.4rem 0;
    font-size: 0.9rem;
    color: var(--text-strong);
  }
  .about-md :global(p) {
    font-size: 0.85rem;
    line-height: 1.4;
    margin: 0.4rem 0;
  }
  .about-md :global(p:first-child) {
    margin-top: 0;
  }
  .about-md :global(ul) {
    margin: 0;
    padding-left: 1.2rem;
    font-size: 0.85rem;
  }
  .about-md :global(li) {
    margin-bottom: 0.35rem;
  }
  .about-md :global(a) {
    color: var(--accent);
    text-decoration: none;
  }
  .about-md :global(a:hover) {
    text-decoration: underline;
  }
  .about-md :global(code) {
    font-family: ui-monospace, Menlo, monospace;
    font-size: 0.82rem;
    background: var(--bg-input, transparent);
    padding: 0 0.2rem;
    border-radius: 2px;
  }
  dl.build {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.25rem 0.75rem;
    margin: 0 0 0.5rem 0;
  }
  dl.build dt {
    font-weight: 600;
    color: var(--text-muted);
    font-size: 0.8rem;
    align-self: center;
  }
  dl.build dd {
    margin: 0;
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }
  .build-id {
    background: var(--bg-input, #000);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.45rem;
    font-family: ui-monospace, Menlo, monospace;
    font-size: 0.82rem;
    user-select: text;
  }
  .build-date {
    color: var(--text-muted);
    font-size: 0.78rem;
    user-select: text;
  }
  .copy-btn {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    color: var(--text-strong);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--border));
    border-radius: 3px;
    padding: 0.15rem 0.6rem;
    font-size: 0.75rem;
    cursor: pointer;
  }
  .copy-btn:hover {
    background: color-mix(in srgb, var(--accent) 32%, transparent);
  }
  .hint {
    margin: 0.3rem 0 0 0;
    color: var(--text-muted);
    font-size: 0.78rem;
  }
  table.libs {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.78rem;
    margin-top: 0.5rem;
  }
  table.libs th,
  table.libs td {
    text-align: left;
    padding: 0.25rem 0.45rem;
    border-bottom: 1px solid var(--border);
    vertical-align: top;
  }
  table.libs th {
    color: var(--text-muted);
    font-weight: 600;
    font-size: 0.75rem;
  }
  table.libs td a {
    color: var(--accent);
    text-decoration: none;
  }
  table.libs td a:hover {
    text-decoration: underline;
  }
  p {
    font-size: 0.85rem;
    line-height: 1.4;
    margin: 0.4rem 0;
  }
  code {
    font-family: ui-monospace, Menlo, monospace;
    font-size: 0.82rem;
    background: var(--bg-input, transparent);
    padding: 0 0.2rem;
    border-radius: 2px;
  }
</style>
