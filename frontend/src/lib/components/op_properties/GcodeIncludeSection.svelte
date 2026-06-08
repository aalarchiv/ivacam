<script lang="ts">
  /// rxm9 GcodeInclude op-properties: file-picker + path display + content
  /// textarea, plus the variable cheat-sheet hint and the xi2g verbose
  /// unsim-warning toggle.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import type { GcodeIncludeOp, OpField, OpFieldValue } from '../../state/project.svelte';

  interface Props {
    op: GcodeIncludeOp;
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();
</script>

<label class="row">
  <span>Name</span>
  <input
    type="text"
    value={op.name}
    oninput={(e) => patch('name', (e.currentTarget as HTMLInputElement).value)}
  />
</label>
<label
  class="row"
  title="Pick a .nc / .ngc / .gcode file. Its contents are loaded into the op (project stays self-contained); the path is kept as a label so you can see what was picked."
>
  <span>File</span>
  <input
    type="file"
    accept=".nc,.ngc,.gcode,.tap,.cnc,text/plain"
    onchange={async (e) => {
      const f = (e.currentTarget as HTMLInputElement).files?.[0];
      if (!f) return;
      const text = await f.text();
      patch('path', f.name);
      patch('content', text);
      // Reset the input so re-picking the same filename re-fires onchange.
      (e.currentTarget as HTMLInputElement).value = '';
    }}
  />
</label>
{#if op.path}
  <p class="hint">
    Loaded from <code>{op.path}</code> · {op.content?.length ?? 0} characters,
    {op.content?.split('\n').length ?? 0} lines.
  </p>
{/if}
<label
  class="row"
  title="The G-code that ships in the program. Edit by hand if you need to tweak after loading. Variable tokens are substituted at Generate time."
>
  <span>Content</span>
  <textarea
    rows="10"
    spellcheck="false"
    value={op.content ?? ''}
    placeholder="G-code text — edit by hand or pick a file above."
    oninput={(e) => patch('content', (e.currentTarget as HTMLTextAreaElement).value)}
    style="font-family: ui-monospace, monospace; white-space: pre;"
  ></textarea>
</label>
<p class="hint hint-pause">
  The pipeline emits the content verbatim at this slot after substituting these tokens:
  <code>{'{x}'}</code> <code>{'{y}'}</code> <code>{'{z}'}</code>
  (last commanded XYZ),
  <code>{'{f}'}</code> (last feed),
  <code>{'{s}'}</code> (last spindle RPM),
  <code>{'{safe_z}'}</code> (this op's fast-Z). Unknown <code>{'{tokens}'}</code> pass through and surface
  a warning. The sim carves G0/G1/G2/G3 and canned cycles G73/G81/G82/G83; anything else fires a counted
  "lines skipped" warning so you know what the heightmap won't show.
</p>
<!-- xi2g: verbose per-line warning toggle. Off by default; users
     debugging an exotic block flip it on to see exactly which
     lines were skipped and why. -->
<label
  class="row"
  title="When on, each unsimulated line fires its own warning with the line offset and reason — useful for debugging an exotic block. When off (default), only the counted summary fires."
>
  <span>Verbose unsim warnings</span>
  <input
    type="checkbox"
    checked={op.verboseUnsimWarnings ?? false}
    onchange={(e) => patch('verboseUnsimWarnings', (e.currentTarget as HTMLInputElement).checked)}
  />
</label>
