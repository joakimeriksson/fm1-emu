// Pure routing policy shared with controller regression tests.
export function selectedInputs(inputs, wanted) {
  if (!wanted) return [];
  if (wanted === "*") return inputs;
  const exact = inputs.filter(input => input.id === wanted);
  if (exact.length) return exact;
  const legacy = inputs.filter(input => input.name === wanted);
  return legacy.length === 1 ? legacy : [];
}
export function panicMessages() {
  return Array.from({length:16}, (_, channel) =>
    [64,123,120].map(controller => [0xb0 | channel,controller,0])).flat();
}
