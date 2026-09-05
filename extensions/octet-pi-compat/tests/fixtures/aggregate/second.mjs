export async function installFakePiAggregate({ eventBus, runtime }) {
  runtime.aggregate.loadOrder.push("second");
  runtime.aggregate.globalMarker = globalThis.__octetPiAggregateShared?.marker ?? "missing";
  eventBus.emit("aggregate:second-ready", "second");
}
