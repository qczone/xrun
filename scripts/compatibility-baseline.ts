/** Frozen, unreleased protocol-1 snapshot used until a compatible tag exists. */
export const DEVELOPMENT_BASELINE = "bc97f764b4c96e07fab2736ee029349389eb0c03";

function version(value: string): string | undefined {
  const match =
    /^(?:v)?((?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?)$/.exec(
      value,
    );
  if (!match) return;
  if (
    match[2]
      ?.split(".")
      .some(
        (part) => /^\d+$/.test(part) && part.length > 1 && part.startsWith("0"),
      )
  )
    return;
  return match[1];
}

/** Earlier release tags in descending SemVer precedence, with stable after beta. */
export function earlierTags(tags: string[], current: string): string[] {
  const release = version(current);
  if (!release) throw new Error(`Invalid current version: ${current}`);
  return tags
    .flatMap((ref) => {
      const parsed = version(ref);
      return parsed && Bun.semver.order(parsed, release) < 0
        ? [{ ref, version: parsed }]
        : [];
    })
    .sort(
      (left, right) =>
        Bun.semver.order(right.version, left.version) ||
        left.ref.localeCompare(right.ref),
    )
    .map((tag) => tag.ref);
}

export function protocolRange(source: string) {
  const max = Number(/pub const PROTOCOL: u32 = (\d+);/.exec(source)?.[1]);
  const min = Number(/min:\s*(\d+)/.exec(source)?.[1]);
  if (
    !Number.isSafeInteger(min) ||
    !Number.isSafeInteger(max) ||
    min <= 0 ||
    max < min
  )
    throw new Error("No implemented protocol range");
  return { min, max };
}

export function overlaps(
  left: { min: number; max: number },
  right: { min: number; max: number },
) {
  return Math.max(left.min, right.min) <= Math.min(left.max, right.max);
}
