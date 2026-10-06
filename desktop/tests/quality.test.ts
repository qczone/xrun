import { expect, test } from "bun:test";
import { sourceProblems } from "../scripts/check-quality";

test("readability counts Unicode characters and rejects only lines over the limit", async () => {
  expect(await sourceProblems("src/example.rs", "好".repeat(150))).toEqual([]);
  expect(
    await sourceProblems("src/example.rs", `${"好".repeat(150)}\r\n`),
  ).toEqual([]);
  expect(await sourceProblems("src/example.rs", "好".repeat(151))).toEqual([
    "src/example.rs:1: line exceeds 150 characters",
  ]);
});

test("relay assertion check ignores negation, strings, comments and definite fields", async () => {
  const source = `const text = "value!";
// promise!.then()
if (!false) console.log(text);
class State { value!: string; }
`;
  expect(await sourceProblems("cloudflare/src/example.ts", source)).toEqual([]);
  expect(
    await sourceProblems(
      "cloudflare/src/example.ts",
      `${source}const x = value!;`,
    ),
  ).toEqual(["cloudflare/src/example.ts:5: non-null assertion is forbidden"]);
  expect(
    await sourceProblems("desktop/src/example.ts", "const x = value!;"),
  ).toEqual([]);
  expect(
    await sourceProblems("cloudflare\\src\\example.ts", "const x = value!;"),
  ).toHaveLength(1);
});

test("relay assertion check sees nested expressions and rejects invalid syntax", async () => {
  expect(
    await sourceProblems("cloudflare/src/example.ts", "const x = (value!)!;"),
  ).toHaveLength(2);
  await expect(
    sourceProblems("cloudflare/src/example.ts", "const = ;"),
  ).rejects.toThrow();
});
