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
if (
  !platform ||
  !["linux-x86_64", "darwin-arm64", "windows-x86_64"].includes(platform)
) {
  throw new Error(
    "Specify --platform linux-x86_64, darwin-arm64 or windows-x86_64",
  );
}
const directory = resolve(values.directory);
const config = Bun.TOML.parse(
  await readFile(resolve(root, "Cargo.toml"), "utf8"),
) as { package: { version: string } };
const version = config.package.version;
const extension = platform === "windows-x86_64" ? "zip" : "tar.gz";
const files: Record<string, string> = {
  cli: `xrun-${platform}.${extension}`,
};
let installer: string | undefined;
if (platform === "darwin-arm64") {
  files.app = "xrun-app-darwin-arm64.zip";
  files.dmg = "xrun-app-darwin-arm64.dmg";
  installer = "install.sh";
} else if (platform === "windows-x86_64") {
  files.app = "xrun-app-windows-x86_64.exe";
  installer = "install.ps1";
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
if (installer) {
  await copyFile(resolve(root, installer), resolve(directory, installer));
}
