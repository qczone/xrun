/** At most two UTC days fit in each hibernating socket's cumulative attachment. */
export const TRAFFIC_DAY = 86_400_000;
export interface TrafficCounter {
  day_ms: number;
  ingress_bytes: number;
  egress_bytes: number;
  first_seen_ms: number;
}
export interface TrafficLeg {
  id: string;
  network: string;
  role: "source" | "target";
  source?: string | null;
  target: string;
  traffic: TrafficCounter[];
}
export function trafficCounters(value: unknown): TrafficCounter[] | undefined {
  if (!Array.isArray(value) || value.length > 2) return;
  const counters: TrafficCounter[] = [];
  for (const item of value) {
    if (
      !item ||
      typeof item !== "object" ||
      Array.isArray(item) ||
      Object.keys(item).some(
        (key) =>
          ![
            "day_ms",
            "ingress_bytes",
            "egress_bytes",
            "first_seen_ms",
          ].includes(key),
      )
    )
      return;
    const cell = item as Record<string, unknown>;
    const day = cell.day_ms;
    const number = (value: unknown): value is number =>
      typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
    if (
      !number(day) ||
      day % TRAFFIC_DAY ||
      !number(cell.ingress_bytes) ||
      !number(cell.egress_bytes) ||
      !number(cell.first_seen_ms) ||
      cell.first_seen_ms < day ||
      cell.first_seen_ms >= day + TRAFFIC_DAY ||
      counters.some((counter) => counter.day_ms >= day)
    )
      return;
    counters.push({
      day_ms: day,
      ingress_bytes: cell.ingress_bytes,
      egress_bytes: cell.egress_bytes,
      first_seen_ms: cell.first_seen_ms,
    });
  }
  return counters;
}
