import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { copyFile, readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { parseArgs } from "node:util";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const { values } = parseArgs({
  args: process.argv.slice(2),
  options: {
    platform: { type: "string" },
    directory: { type: "string", default: "dist" },
  },
});
const platform = values.platform;
if (!platform || !/^(linux|darwin|windows)-(x86_64|arm64)$/.test(platform)) {
  throw new Error("Specify --platform <linux|darwin|windows>-<x86_64|arm64>");
}
const directory = resolve(values.directory);
const config = Bun.TOML.parse(
  await readFile(resolve(root, "Cargo.toml"), "utf8"),
) as { package: { version: string } };
const version = config.package.version;
const files: Record<string, string> = {};
let installer = "install.sh";
if (platform.startsWith("darwin-")) {
  files.app = `xrun-app-${platform}.zip`;
} else if (platform.startsWith("windows-")) {
  files.app = `xrun-app-${platform}.exe`;
  installer = "install.ps1";
} else {
  files.cli = `xrun-${platform}.tar.gz`;
}
const artifacts: Record<string, { file: string; sha256: string }> = {};
for (const [component, file] of Object.entries(files)) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(resolve(directory, file))) {
    hash.update(chunk);
  }
  artifacts[component] = { file, sha256: hash.digest("hex") };
}
await writeFile(
  resolve(directory, `xrun-${platform}.json`),
  `${JSON.stringify({ schema: 1, version, platform, artifacts }, null, 2)}\n`,
);
await copyFile(
  resolve(root, "scripts", installer),
  resolve(directory, installer),
);
