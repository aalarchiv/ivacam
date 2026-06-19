import type { ProjectState } from './project.svelte';
import type { PickerKind } from '../components/OpKindPicker.svelte';

/// Create a new operation from the current object selection and route it
/// through the history bus as a single undoable transaction. The
/// synthetic `pocket_outside` kind expands to a regular Pocket pre-filled
/// with a rectangular frame and difference-combine; every other kind maps
/// straight to `addOperation`.
///
/// Shared verbatim by the 2D (`EntityCanvas2D`) and 3D (`Scene3D`) canvas
/// context menus. `kindLabel` is the picker's display label for `kind`
/// (e.g. "Pocket") — passed in so this helper stays free of the UI label
/// map. Callers are responsible for checking the selection is non-empty
/// and for any post-create UI (e.g. bouncing the sidebar to Operations).
export function createOpFromSelection(
  project: ProjectState,
  kind: PickerKind,
  kindLabel: string,
  sel: number[],
): void {
  project.history.beginTransaction(`New ${kindLabel} from selection`);
  try {
    if (kind === 'pocket_outside') {
      const endmill = project.data.tools.find((t) => t.kind === 'endmill') ?? project.data.tools[0];
      const toolDiameter = endmill?.diameter ?? 3;
      const op = project.addOperation('pocket');
      project.updateOperation(op.id, {
        name: 'Pocket Outside',
        toolId: endmill?.id ?? op.toolId,
        sourceLayers: null,
        sourceObjects: sel,
        sourceCombine: 'difference',
        frameShape: 'rectangle',
        framePaddingMm: 3 * toolDiameter,
        frameCornerRadiusMm: undefined,
      });
    } else {
      const op = project.addOperation(kind);
      project.updateOperation(op.id, {
        name: `${kindLabel} from selection`,
        sourceLayers: null,
        sourceObjects: sel,
      });
    }
    project.history.commitTransaction();
  } catch (e) {
    project.cancelTransaction();
    throw e;
  }
}
