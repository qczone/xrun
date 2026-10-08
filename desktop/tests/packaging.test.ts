import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import {
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { buildOptions } from "../scripts/desktop";

const root = resolve(import.meta.dir, "../..");
const targetDir = resolve("target");
const macHost = "aarch64-apple-darwin";
const macIntel = "x86_64-apple-darwin";

test("desktop target selection keeps App and helper paths aligned", () => {
  const native = buildOptions(["dev"], macHost, targetDir, "darwin");
  expect(native.profileDir).toBe(join(targetDir, "debug"));
  expect(native.targetArgs).toEqual([]);
  for (const option of [
    ["--target", macIntel],
    [`--target=${macIntel}`],
    ["-t", macIntel],
  ]) {
    const cross = buildOptions(
      ["build", ...option, "--debug", "--no-bundle", "--", "--offline"],
      macHost,
      targetDir,
      "darwin",
    );
    expect(cross.target).toBe(macIntel);
    expect(cross.targetArgs).toEqual(["--target", macIntel]);
    expect(cross.cliArgs).toEqual([
      "--debug",
      "--no-bundle",
      "--target",
      macIntel,
    ]);
    expect(cross.cargoArgs).toEqual(["--offline"]);
    expect(cross.profileDir).toBe(join(targetDir, macIntel, "debug"));
  }
  const fromEnvironment = buildOptions(
    ["build"],
    macHost,
    targetDir,
    "darwin",
    macIntel,
  );
  expect(fromEnvironment.profileDir).toBe(join(targetDir, macIntel, "release"));
  expect(fromEnvironment.cliArgs).toEqual(["--target", macIntel]);
  const override = buildOptions(
    ["build", "--target", macHost],
    macHost,
    targetDir,
    "darwin",
    macIntel,
  );
  expect(override.target).toBe(macHost);
  for (const target of ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"]) {
    const windows = buildOptions(
      ["build", "--target", target],
      target,
      targetDir,
      "win32",
    );
    expect(windows.profileDir).toBe(join(targetDir, target, "release"));
    expect(windows.targetArgs).toEqual(["--target", target]);
  }
});

test("invalid and misplaced target flags fail before compiling a mismatched helper", () => {
  for (const args of [
    ["build", "--target"],
    ["build", "--target="],
    ["build", "--target", macIntel, "--target", macHost],
    ["build", "--", "--target", macIntel],
    ["build", "--target", "aarch64-pc-windows-msvc"],
    ["build", "--target", "universal-apple-darwin"],
  ]) {
    expect(() => buildOptions(args, macHost, targetDir, "darwin")).toThrow();
  }
});

test.skipIf(process.platform !== "darwin")(
  "cross-build wrapper passes one target to Cargo and Tauri and copies that helper",
  async () => {
    const temp = await mkdtemp(join(tmpdir(), "xrun-cross-build-"));
    try {
      const desktop = join(temp, "desktop");
      const bin = join(temp, "bin");
      await mkdir(join(desktop, "scripts"), { recursive: true });
      await mkdir(join(desktop, "node_modules/@tauri-apps/cli"), {
        recursive: true,
      });
      await mkdir(bin);
      await copyFile(
        join(root, "desktop/scripts/desktop.ts"),
        join(desktop, "scripts/desktop.ts"),
      );
      await writeFile(
        join(bin, "rustc"),
        `#!/usr/bin/env bun\nconsole.log("host: ${macHost}");\n`,
        { mode: 0o755 },
      );
      await writeFile(
        join(bin, "cargo"),
        `#!/usr/bin/env bun
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
const args = process.argv.slice(2);
const target = args[args.indexOf("--target") + 1];
if (target !== "${macIntel}") throw new Error("helper must use the requested target");
const dir = join(process.env.CARGO_TARGET_DIR, target, "debug");
mkdirSync(dir, { recursive: true });
writeFileSync(join(dir, "xrun"), "helper for " + target);
`,
        { mode: 0o755 },
      );
      await writeFile(
        join(desktop, "node_modules/@tauri-apps/cli/tauri.js"),
        `
import { readFileSync, writeFileSync } from "node:fs";
const args = process.argv.slice(2);
const target = args[args.indexOf("--target") + 1];
const helper = readFileSync("src-tauri/binaries/xrun-" + target, "utf8");
if (target !== "${macIntel}" || helper !== "helper for " + target) throw new Error("App/helper architecture mismatch");
writeFileSync("target-used", target);
`,
      );
      const child = Bun.spawn(
        [
          process.execPath,
          join(desktop, "scripts/desktop.ts"),
          "build",
          "--target",
          macIntel,
          "--debug",
          "--no-bundle",
        ],
        {
          cwd: temp,
          env: {
            ...process.env,
            CARGO_TARGET_DIR: join(temp, "target"),
            CARGO_BUILD_TARGET: macHost,
            PATH: `${bin}:${dirname(process.execPath)}:${process.env.PATH}`,
          },
          stdout: "pipe",
          stderr: "pipe",
        },
      );
      const error = await new Response(child.stderr).text();
      expect(await child.exited, error).toBe(0);
      expect(await readFile(join(desktop, "target-used"), "utf8")).toBe(
        macIntel,
      );
    } finally {
      await rm(temp, { recursive: true, force: true });
    }
  },
);

test("release manifests identify and hash all six platform artifact sets", async () => {
  const temp = await mkdtemp(join(tmpdir(), "xrun-manifests-"));
  try {
    for (const os of ["linux", "darwin", "windows"]) {
      for (const arch of ["x86_64", "arm64"]) {
        const platform = `${os}-${arch}`;
        const directory = join(temp, platform);
        await mkdir(directory);
        const files: Record<string, string> =
          os === "linux"
            ? { cli: `xrun-${platform}.tar.gz` }
            : {
                app: `xrun-app-${platform}.${os === "darwin" ? "zip" : "exe"}`,
              };
        for (const file of Object.values(files))
          await writeFile(join(directory, file), file);
        const child = Bun.spawn(
          [
            process.execPath,
            join(root, "desktop/scripts/package-manifest.ts"),
            "--platform",
            platform,
            "--directory",
            directory,
          ],
          { stdout: "pipe", stderr: "pipe" },
        );
        const error = await new Response(child.stderr).text();
        expect(await child.exited, error).toBe(0);
        const manifest = await Bun.file(
          join(directory, `xrun-${platform}.json`),
        ).json();
        expect(manifest.schema).toBe(1);
        expect(manifest.platform).toBe(platform);
        expect(manifest.version).toBe(
          (await Bun.file(join(root, "desktop/package.json")).json()).version,
        );
        expect(Object.keys(manifest.artifacts).sort()).toEqual(
          Object.keys(files).sort(),
        );
        for (const [component, file] of Object.entries(files)) {
          expect(manifest.artifacts[component]).toEqual({
            file,
            sha256: createHash("sha256").update(file).digest("hex"),
          });
        }
        const installer = os === "windows" ? "install.ps1" : "install.sh";
        expect(await readFile(join(directory, installer), "utf8")).toBe(
          await readFile(join(root, "scripts", installer), "utf8"),
        );
      }
    }
    const missing = join(temp, "missing");
    await mkdir(missing);
    await writeFile(join(missing, "xrun-darwin-x86_64.tar.gz"), "cli only");
    const failed = Bun.spawn(
      [
        process.execPath,
        join(root, "desktop/scripts/package-manifest.ts"),
        "--platform",
        "darwin-x86_64",
        "--directory",
        missing,
      ],
      { stdout: "ignore", stderr: "ignore" },
    );
    expect(await failed.exited).not.toBe(0);
    expect(
      await Bun.file(join(missing, "xrun-darwin-x86_64.json")).exists(),
    ).toBe(false);
  } finally {
    await rm(temp, { recursive: true, force: true });
  }
});
