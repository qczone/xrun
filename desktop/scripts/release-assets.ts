import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import {
  copyFile,
  mkdir,
  readFile,
  readdir,
  writeFile,
} from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

async function sha256(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

async function releaseVersion(): Promise<string> {
  const versions: string[] = [];
  for (const file of ["Cargo.toml", "desktop/src-tauri/Cargo.toml"]) {
    const config = Bun.TOML.parse(await readFile(join(root, file), "utf8")) as {
      package: { version: string };
    };
    versions.push(config.package.version);
  }
  for (const file of [
    "desktop/package.json",
    "desktop/src-tauri/tauri.conf.json",
  ]) {
    versions.push(JSON.parse(await readFile(join(root, file), "utf8")).version);
  }
  const lock = Bun.TOML.parse(
    await readFile(join(root, "Cargo.lock"), "utf8"),
  ) as { package: { name: string; version: string }[] };
  for (const name of ["xrun", "xrun-desktop"]) {
    const pkg = lock.package.find((pkg) => pkg.name === name);
    if (!pkg) throw new Error(`Missing ${name} in Cargo.lock`);
    versions.push(pkg.version);
  }
  const version = versions[0];
  if (
    !version ||
    !/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$/.test(version) ||
    versions.some((value) => value !== version)
  ) {
    throw new Error("Synchronize CLI, desktop, lockfile and Tauri versions");
  }
  return version;
}

/** Assemble only the six verified downloads and their installation metadata. */
export async function prepareRelease(
  artifacts: string,
  output: string,
  commit: string,
): Promise<string> {
  if (!/^[0-9a-f]{40}$/.test(commit))
    throw new Error("Specify a full commit SHA");
  const version = await releaseVersion();
  const files = new Map<string, { source: string; digest: string }>();
  for (const os of ["linux", "darwin", "windows"]) {
    for (const arch of ["x86_64", "arm64"]) {
      const platform = `${os}-${arch}`;
      const directory = join(artifacts, `xrun-${platform}-${commit}`);
      const name = `xrun-${platform}.json`;
      const source = join(directory, name);
      const manifest = JSON.parse(await readFile(source, "utf8"));
      const component = os === "linux" ? "cli" : "app";
      const file =
        os === "linux"
          ? `xrun-${platform}.tar.gz`
          : `xrun-app-${platform}.${os === "darwin" ? "zip" : "exe"}`;
      const entry = manifest.artifacts?.[component];
      if (
        manifest.schema !== 1 ||
        manifest.version !== version ||
        manifest.platform !== platform ||
        Object.keys(manifest.artifacts ?? {}).join() !== component ||
        entry?.file !== file ||
        !/^[0-9a-f]{64}$/.test(entry?.sha256 ?? "")
      ) {
        throw new Error(`Invalid release manifest: ${name}`);
      }
      const artifact = join(directory, file);
      if ((await sha256(artifact)) !== entry.sha256)
        throw new Error(`Release checksum mismatch: ${file}`);
      files.set(file, { source: artifact, digest: entry.sha256 });
      files.set(name, { source, digest: await sha256(source) });
      const installer = os === "windows" ? "install.ps1" : "install.sh";
      if (
        (await sha256(join(directory, installer))) !==
        (await sha256(join(root, "scripts", installer)))
      ) {
        throw new Error(`Installer does not match this commit: ${platform}`);
      }
    }
  }
  for (const file of ["install.sh", "install.ps1", "LICENSE"]) {
    const source = join(root, file === "LICENSE" ? file : `scripts/${file}`);
    files.set(file, { source, digest: await sha256(source) });
  }
  await mkdir(output, { recursive: true });
  if ((await readdir(output)).length)
    throw new Error("Release output directory must be empty");
  const checksums: string[] = [];
  for (const [file, { source, digest }] of [...files.entries()].sort()) {
    await copyFile(source, join(output, file));
    checksums.push(`${digest}  ${file}`);
  }
  await writeFile(join(output, "SHA256SUMS"), `${checksums.join("\n")}\n`);
  const repository = process.env.GITHUB_REPOSITORY ?? "qczone/xrun";
  const base = `https://github.com/${repository}/releases/download/v${version}`;
  const downloads = [
    ["Mac · Apple Silicon", "xrun-app-darwin-arm64.zip"],
    ["Mac · Intel", "xrun-app-darwin-x86_64.zip"],
    ["Windows · x86_64", "xrun-app-windows-x86_64.exe"],
    ["Windows · ARM64", "xrun-app-windows-arm64.exe"],
    ["Linux · x86_64", "xrun-linux-x86_64.tar.gz"],
    ["Linux · ARM64", "xrun-linux-arm64.tar.gz"],
  ]
    .map(([platform, file]) => `| ${platform} | [${file}](${base}/${file}) |`)
    .join("\n");
  await writeFile(
    resolve(output, "../release-notes.md"),
    `macOS：下载对应架构的 App ZIP，解压后打开 xrun.app。Windows：运行对应架构的安装 EXE。两者均包含终端 CLI。Linux：CLI 压缩包内仅包含 xrun。\n\n` +
      `| 平台 | 下载 |\n| --- | --- |\n${downloads}\n\n` +
      `每个平台提供 x86_64 / arm64 两种架构。安装脚本自动选择设备架构并验证版本与 SHA-256。\n\n` +
      `macOS / Linux 命令安装：\n\n\`\`\`bash\ncurl -fL '${base}/install.sh' -o install.sh\nbash install.sh --version ${version}\n\`\`\`\n\n` +
      `Windows PowerShell 命令安装：\n\n\`\`\`powershell\nInvoke-WebRequest '${base}/install.ps1' -OutFile install.ps1\npowershell.exe -NoProfile -ExecutionPolicy Bypass -File .\\install.ps1 -Version ${version}\n\`\`\`\n\n` +
      `完整说明：[使用手册](https://github.com/${repository}/blob/v${version}/docs/usage.md#安装)。\n`,
  );
  return version;
}

if (import.meta.main) {
  const { values } = parseArgs({
    args: process.argv.slice(2),
    options: {
      artifacts: { type: "string", default: "dist/artifacts" },
      directory: { type: "string", default: "dist/release" },
      sha: { type: "string" },
    },
  });
  console.log(
    await prepareRelease(
      resolve(values.artifacts),
      resolve(values.directory),
      values.sha ?? "",
    ),
  );
}
