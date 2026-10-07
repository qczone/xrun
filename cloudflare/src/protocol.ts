/** Release-independent wire contract. Keep aligned with Rust protocol/version.rs. */
export const PROTOCOL = { min: 1, max: 2 } as const;
export const PROTOCOL_HEADER = `${PROTOCOL.min}-${PROTOCOL.max}`;
export const SIGNATURE_FORMAT = 1;
export interface ProtocolRange {
  min: number;
  max: number;
}
function valid(range: ProtocolRange): boolean {
  return (
    Number.isSafeInteger(range.min) &&
    Number.isSafeInteger(range.max) &&
    range.min > 0 &&
    range.min <= range.max &&
    range.max <= 0xffff_ffff
  );
}
export function negotiate(local: ProtocolRange, peer: ProtocolRange): number {
  const selected = Math.min(local.max, peer.max);
  if (!valid(local) || !valid(peer) || selected < Math.max(local.min, peer.min))
    throw new Error("No common supported protocol");
  return selected;
}
export function parseRange(value: string | null): ProtocolRange {
  const match = value?.match(/^(\d+)-(\d+)$/);
  if (!match) throw new Error("Missing or invalid protocol range");
  const range = { min: Number(match[1]), max: Number(match[2]) };
  negotiate(range, range);
  return range;
}
