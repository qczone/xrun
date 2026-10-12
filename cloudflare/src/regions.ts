import type { DurableObjectLocationHint } from "@cloudflare/workers-types";
/** Placement hints affect only an object's first creation, never authorization. */
export const REGIONS = [
  "wnam",
  "enam",
  "sam",
  "weur",
  "eeur",
  "apac",
  "apac-ne",
  "apac-se",
  "oc",
  "afr",
  "me",
] as const;
export function region(value: unknown): DurableObjectLocationHint | undefined {
  return typeof value === "string" &&
    (REGIONS as readonly string[]).includes(value)
    ? (value as DurableObjectLocationHint)
    : undefined;
}
export function edgeRegion(
  continent: unknown,
): DurableObjectLocationHint | undefined {
  return region(
    (
      {
        NA: "enam",
        SA: "sam",
        EU: "weur",
        AS: "apac",
        OC: "oc",
        AF: "afr",
      } as Record<string, string>
    )[String(continent)],
  );
}
