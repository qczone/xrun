import { readdir, readFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parsers } from "prettier/plugins/typescript";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const directories = [
  "src",
  "desktop/src-tauri/src",
  "desktop/src",
  "cloudflare/src",
];
const problems: string[] = [];

function walkAssertions(node: unknown, report: (line: number) => void): void {
  if (!node || typeof node !== "object") return;
  if (Array.isArray(node)) {
    for (const child of node) walkAssertions(child, report);
    return;
  }
  const value = node as Record<string, unknown>;
  if (value.type === "TSNonNullExpression") {
    const location = value.loc as { start: { line: number } };
    report(location.start.line);
  }
  for (const [key, child] of Object.entries(value)) {
    if (!["loc", "range", "tokens", "comments"].includes(key))
      walkAssertions(child, report);
  }
}

/** Check source text with the same rules used by the CI entry point. */
export async function sourceProblems(
  path: string,
  source: string,
): Promise<string[]> {
  const findings: string[] = [];
  source.split(/\r?\n/).forEach((line, index) => {
    if ([...line].length > 150)
      findings.push(`${path}:${index + 1}: line exceeds 150 characters`);
  });
  const normalizedPath = path.replaceAll("\\", "/");
  if (
    normalizedPath.startsWith("cloudflare/src/") &&
    normalizedPath.endsWith(".ts")
  ) {
    // Standalone parsing needs only filepath; Prettier's declaration also requires
    // the formatter's unrelated print options, which its TS parser does not use.
    const options = { filepath: path } as Parameters<
      typeof parsers.typescript.parse
    >[1];
    const ast: unknown = await parsers.typescript.parse(source, options);
    walkAssertions(ast, (line) =>
      findings.push(`${path}:${line}: non-null assertion is forbidden`),
    );
  }
  return findings;
}

async function scan(directory: string): Promise<void> {
  for (const entry of await readdir(join(root, directory), {
    withFileTypes: true,
  })) {
    const path = `${directory}/${entry.name}`;
    if (entry.isDirectory()) {
      await scan(path);
    } else if (/\.(rs|tsx?)$/.test(entry.name)) {
      const source = await readFile(join(root, path), "utf8");
      problems.push(...(await sourceProblems(path, source)));
    }
  }
}
if (import.meta.main) {
  for (const directory of directories) await scan(directory);
  for (const problem of problems) console.error(problem);
  if (problems.length) process.exitCode = 1;
  console.log(`Readability checks: ${problems.length} findings`);
}
