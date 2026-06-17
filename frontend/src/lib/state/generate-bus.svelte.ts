/// Tiny request bus so UI outside GenerateBar can trigger a (re-)generate
/// without prop-threading. The pull-to-refresh gesture (7jug.12) bumps the
/// sequence; GenerateBar watches it and runs its own generate pipeline
/// (the single owner of the run logic), gated on an idle pipeline.

class GenerateBus {
  #seq = $state(0);

  /// Monotonic request counter — a fresh value asks for a generate.
  get seq(): number {
    return this.#seq;
  }

  /// Request a (re-)generate. Coalesces naturally: callers just bump the
  /// counter; the consumer decides whether the pipeline is idle enough to
  /// act.
  request(): void {
    this.#seq++;
  }
}

export const generateBus = new GenerateBus();
