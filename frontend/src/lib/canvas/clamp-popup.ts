/// Svelte action: keep an absolutely-positioned popup (context menu /
/// popover with inline `left`/`top` styles) fully inside its
/// offsetParent. The canvas containers are `overflow: hidden`, so a
/// menu spawned near the right/bottom edge would otherwise be CLIPPED
/// at the panel boundary (reading as "hidden under the sidebar").
/// Measure-based — works for any menu height, unlike the footprint
/// estimates it replaces. Pass the anchor state as the action
/// parameter so re-opening at a new position re-clamps the same node.
export function clampPopup(node: HTMLElement, _anchor?: unknown) {
  const apply = () => {
    const host = node.offsetParent as HTMLElement | null;
    if (!host) return;
    const margin = 8;
    const maxX = host.clientWidth - node.offsetWidth - margin;
    const maxY = host.clientHeight - node.offsetHeight - margin;
    const x = Math.max(margin, Math.min(node.offsetLeft, Math.max(margin, maxX)));
    const y = Math.max(margin, Math.min(node.offsetTop, Math.max(margin, maxY)));
    node.style.left = `${x}px`;
    node.style.top = `${y}px`;
  };
  // After layout, so offsetWidth/Height are real.
  queueMicrotask(apply);
  return {
    update() {
      queueMicrotask(apply);
    },
  };
}
