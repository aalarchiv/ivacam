/// Leading + trailing throttle keyed by string. The first call for a key
/// runs immediately (instant feedback); further calls within `intervalMs`
/// are collapsed so only the MOST RECENT runs, on the trailing edge.
///
/// Used to cap the rate of expensive recomputes during a spinbox hold /
/// drag — e.g. a file-transform edit re-transforms every segment of an
/// import and re-uploads the GPU buffer, so firing it per input event on
/// a 100k-segment DXF stutters. Throttling collapses a burst to ~30
/// recomputes/second while still guaranteeing the final value lands.
export class Throttle {
  private timers = new Map<string, ReturnType<typeof setTimeout>>();
  private trailing = new Map<string, () => void>();

  constructor(private readonly intervalMs: number) {}

  /// Run `fn` for `key`, subject to throttling. Leading edge runs
  /// synchronously; subsequent calls within the window overwrite the
  /// pending trailing call so only the latest value is applied.
  run(key: string, fn: () => void): void {
    if (this.timers.has(key)) {
      this.trailing.set(key, fn);
      return;
    }
    fn();
    this.open(key);
  }

  private open(key: string): void {
    this.timers.set(
      key,
      setTimeout(() => {
        this.timers.delete(key);
        const t = this.trailing.get(key);
        if (t) {
          this.trailing.delete(key);
          t();
          // Keep the window alive while a burst continues so a sustained
          // drag still fires at most once per interval.
          this.open(key);
        }
      }, this.intervalMs),
    );
  }

  /// Apply any pending trailing calls immediately and clear all timers.
  /// Use on commit (blur / Enter) or teardown so the final value isn't
  /// lost waiting for the trailing edge.
  flush(): void {
    const pending = [...this.trailing.values()];
    for (const h of this.timers.values()) clearTimeout(h);
    this.timers.clear();
    this.trailing.clear();
    for (const fn of pending) fn();
  }

  /// Drop pending trailing calls without running them and clear timers.
  cancel(): void {
    for (const h of this.timers.values()) clearTimeout(h);
    this.timers.clear();
    this.trailing.clear();
  }
}
