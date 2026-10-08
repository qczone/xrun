import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import {
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { prepareRelease } from "../scripts/release-assets";

const root = resolve(import.meta.dir, "../..");
const commit = "a".repeat(40);

async function fixture() {
  const temp = await mkdtemp(join(tmpdir(), "xrun-release-"));
  const artifacts = join(temp, "artifacts");
  const output = join(temp, "release");
  const version = (await Bun.file(join(root, "desktop/package.json")).json())
    .version;
  const manifests = new Map<string, string>();
  for (const os of ["linux", "darwin", "windows"]) {
    for (const arch of ["x86_64", "arm64"]) {
      const platform = `${os}-${arch}`;
      const directory = join(artifacts, `xrun-${platform}-${commit}`);
      await mkdir(directory, { recursive: true });
      const component = os === "linux" ? "cli" : "app";
      const file =
        os === "linux"
          ? `xrun-${platform}.tar.gz`
          : `xrun-app-${platform}.${os === "darwin" ? "zip" : "exe"}`;
      await writeFile(join(directory, file), platform);
      const manifest = join(directory, `xrun-${platform}.json`);
      await writeFile(
        manifest,
        JSON.stringify({
          schema: 1,
          version,
          platform,
          artifacts: {
            [component]: {
              file,
              sha256: createHash("sha256").update(platform).digest("hex"),
            },
          },
        }),
      );
      const installer = os === "windows" ? "install.ps1" : "install.sh";
      await copyFile(
        join(root, "scripts", installer),
        join(directory, installer),
      );
      manifests.set(platform, manifest);
    }
  }
  return { temp, artifacts, output, version, manifests };
}

test("Release contains six downloads, manifests, installers, license and verifiable checksums", async () => {
  const f = await fixture();
  try {
    const child = Bun.spawn(
      [
        process.execPath,
        join(root, "desktop/scripts/release-assets.ts"),
        "--artifacts",
        f.artifacts,
        "--directory",
        f.output,
        "--sha",
        commit,
      ],
      { stdout: "pipe", stderr: "pipe" },
    );
    const stdout = await new Response(child.stdout).text();
    const stderr = await new Response(child.stderr).text();
    expect(await child.exited, stderr).toBe(0);
    expect(stdout.trim()).toBe(f.version);
    const files = (await readdir(f.output)).sort();
    expect(files).toHaveLength(16);
    expect(files.filter((name) => name.endsWith(".zip"))).toEqual([
      "xrun-app-darwin-arm64.zip",
      "xrun-app-darwin-x86_64.zip",
    ]);
    expect(files.some((name) => name.endsWith(".dmg"))).toBe(false);
    const checksums = await readFile(join(f.output, "SHA256SUMS"), "utf8");
    for (const line of checksums.trim().split("\n")) {
      const [digest, file] = line.split("  ");
      expect(digest).toBe(
        createHash("sha256")
          .update(await readFile(join(f.output, file)))
          .digest("hex"),
      );
    }
    expect(checksums.trim().split("\n")).toHaveLength(files.length - 1);
    const notes = await readFile(join(f.temp, "release-notes.md"), "utf8");
    expect(notes).toContain(`/v${f.version}/install.sh`);
    expect(notes).toContain(`-Version ${f.version}`);
  } finally {
    await rm(f.temp, { recursive: true, force: true });
  }
});

for (const failure of [
  "missing platform",
  "wrong version",
  "wrong artifact",
  "changed download",
  "changed installer",
]) {
  test(`Release preparation rejects ${failure} before writing assets`, async () => {
    const f = await fixture();
    try {
      const manifestPath = f.manifests.get("darwin-arm64");
      if (!manifestPath) throw new Error("Missing fixture manifest");
      const directory = resolve(manifestPath, "..");
      if (failure === "missing platform") {
        await rm(directory, { recursive: true });
      } else if (failure === "changed download") {
        await writeFile(
          join(directory, "xrun-app-darwin-arm64.zip"),
          "changed",
        );
      } else if (failure === "changed installer") {
        await writeFile(join(directory, "install.sh"), "changed");
      } else {
        const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
        if (failure === "wrong version") manifest.version = "9.9.9";
        else manifest.artifacts.app.file = "xrun-app-darwin-x86_64.zip";
        await writeFile(manifestPath, JSON.stringify(manifest));
      }
      await expect(
        prepareRelease(f.artifacts, f.output, commit),
      ).rejects.toThrow();
      expect(await Bun.file(join(f.output, "SHA256SUMS")).exists()).toBe(false);
    } finally {
      await rm(f.temp, { recursive: true, force: true });
    }
  });
}
