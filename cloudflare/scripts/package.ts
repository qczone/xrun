import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { parseArgs } from "node:util";
import { PROTOCOL } from "../src/protocol";

const root = resolve(import.meta.dir, "..");

/** Produce the platform-independent Worker embedded in the released xrun CLI. */
export async function packageRelay(directory: string): Promise<void> {
  const cargo = Bun.TOML.parse(
    await readFile(resolve(root, "../Cargo.toml"), "utf8"),
  ) as { package: { version: string } };
  const source = await readFile(resolve(root, ".wrangler/build/index.js"));
  const config = JSON.parse(
    await readFile(resolve(root, "wrangler.jsonc"), "utf8"),
  );
  const manifest = {
    schema: 1,
    version: cargo.package.version,
    sha256: createHash("sha256").update(source).digest("hex"),
    compatibility_date: config.compatibility_date,
    protocol: PROTOCOL,
  };
  await mkdir(directory, { recursive: true });
  await writeFile(resolve(directory, "relay.mjs"), source);
  await writeFile(
    resolve(directory, "manifest.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
  );
}

if (import.meta.main) {
  const { values } = parseArgs({
    args: Bun.argv.slice(2),
    options: { directory: { type: "string", default: ".wrangler/package" } },
  });
  await import("./build");
  await packageRelay(resolve(root, values.directory));
}
