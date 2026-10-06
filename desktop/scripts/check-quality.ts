import { readdir, readFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const directories = [
  "src",
  "desktop/src-tauri/src",
  "desktop/src",
  "cloudflare/src",
];
const problems: string[] = [];

async function scan(directory: string): Promise<void> {
  for (const entry of await readdir(join(root, directory), {
    withFileTypes: true,
  })) {
    const path = `${directory}/${entry.name}`;
    if (entry.isDirectory()) {
      await scan(path);
    } else if (/\.(rs|tsx?)$/.test(entry.name)) {
      const source = await readFile(join(root, path), "utf8");
      source.split("\n").forEach((line, index) => {
        if ([...line].length > 150)
          problems.push(`${path}:${index + 1}: line exceeds 150 characters`);
      });
    }
  }
}
for (const directory of directories) await scan(directory);
for (const problem of problems) console.error(problem);
if (problems.length && !process.argv.includes("--warn")) process.exitCode = 1;
console.log(`Readability checks: ${problems.length} findings`);
