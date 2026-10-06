import {
  chmod,
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// Compare same-version CLI builds in separate temporary homes. This measures CPU,
// macOS interrupt wakeups, complete log writes and jobs latency without privileged
// tracing; it does not infer file-read or fsync syscall counts from output chunks.
const root = resolve(import.meta.dir, "../..");
const baseline = process.argv[2];
if (!baseline)
  throw new Error(
    "Usage: bun desktop/scripts/benchmark-core.ts <baseline git ref>",
  );
const temporary = await mkdtemp(join(tmpdir(), "xrun-core-benchmark-"));
const env = {
  ...process.env,
  CARGO_TARGET_DIR: join(root, "target"),
  CARGO_PROFILE_DEV_DEBUG: "0",
  CARGO_PROFILE_TEST_DEBUG: "0",
  CARGO_INCREMENTAL: "0",
};
async function run(args: string[], cwd = root): Promise<string> {
  const child = Bun.spawn(args, {
    cwd,
    env,
    stdout: "pipe",
    stderr: "inherit",
  });
  const output = await new Response(child.stdout).text();
  if ((await child.exited) !== 0)
    throw new Error(`${args[0]} ${args[1]} failed`);
  return output;
}
try {
  const reference = (
    await run(["git", "rev-parse", "--verify", `${baseline}^{commit}`])
  ).trim();
  const checkout = join(temporary, "baseline");
  await mkdir(checkout);
  const archive = Bun.spawn(["git", "archive", reference], {
    cwd: root,
    stdout: "pipe",
    stderr: "inherit",
  });
  const extraction = Bun.spawn(["tar", "-x", "-C", checkout], {
    cwd: root,
    stdin: archive.stdout,
    stdout: "inherit",
    stderr: "inherit",
  });
  if ((await extraction.exited) !== 0 || (await archive.exited) !== 0)
    throw new Error("Could not extract the benchmark baseline");
  // Distinct crate and binary names prevent baseline outputs from overwriting
  // current-package fingerprints while reusing already built dependencies.
  const manifest = join(checkout, "Cargo.toml");
  await writeFile(
    manifest,
    (await readFile(manifest, "utf8")).replace(
      "[package]",
      "[package]\nautobins = false",
    ) +
      '\n[lib]\nname = "xrun_benchmark_baseline"\n' +
      '\n[[bin]]\nname = "xrun-benchmark-baseline"\npath = "src/main.rs"\n',
  );
  const entry = join(checkout, "src/main.rs");
  await writeFile(
    entry,
    (await readFile(entry, "utf8")).replaceAll(
      "xrun::",
      "xrun_benchmark_baseline::",
    ),
  );
  await run(["cargo", "build", "--locked", "-p", "xrun"], checkout);
  const binaryName = process.platform === "win32" ? "xrun.exe" : "xrun";
  const baselineBinary = join(temporary, `baseline-${binaryName}`);
  const baselineName =
    process.platform === "win32"
      ? "xrun-benchmark-baseline.exe"
      : "xrun-benchmark-baseline";
  await copyFile(join(root, "target/debug", baselineName), baselineBinary);
  await chmod(baselineBinary, 0o700);
  const build = await run([
    "cargo",
    "test",
    "--locked",
    "--test",
    "performance",
    "--no-run",
    "--message-format=json-render-diagnostics",
  ]);
  const artifacts = build
    .split("\n")
    .filter(Boolean)
    .map(
      (line) =>
        JSON.parse(line) as {
          reason: string;
          target?: { name: string; kind: string[] };
          executable?: string;
        },
    );
  const testBinary = artifacts.find(
    (item) =>
      item.reason === "compiler-artifact" &&
      item.target?.name === "performance" &&
      item.executable,
  )?.executable;
  const currentBinary = join(root, "target/debug", binaryName);
  if (!testBinary)
    throw new Error("Cargo did not report the benchmark executable");
  if (
    (await run([baselineBinary, "--version"])).trim() !==
    (await run([currentBinary, "--version"])).trim()
  )
    throw new Error("Use a baseline with the same protocol/package version");
  const measurements: Record<string, unknown> = {};
  for (const [label, binary] of [
    ["baseline", baselineBinary],
    ["current", currentBinary],
  ]) {
    const output = join(temporary, `${label}.json`);
    const child = Bun.spawn(
      [testBinary, "--ignored", "--nocapture", "--test-threads=1"],
      {
        cwd: root,
        env: { ...env, XRUN_TEST_BINARY: binary, XRUN_BENCH_OUTPUT: output },
        stdout: "inherit",
        stderr: "inherit",
      },
    );
    if ((await child.exited) !== 0)
      throw new Error(`${label} measurement failed`);
    measurements[label] = JSON.parse(await readFile(output, "utf8"));
  }
  const outputDirectory = join(root, "output/optimization-20261006");
  await mkdir(outputDirectory, { recursive: true });
  const output = join(
    outputDirectory,
    `core-benchmark${process.env.XRUN_BENCH_LOG_MODE === "bursts" ? "-bursts" : ""}-${process.platform}.json`,
  );
  await writeFile(
    output,
    JSON.stringify({ baseline_ref: reference, ...measurements }, null, 2) +
      "\n",
  );
  console.log(`Core benchmark written to ${output}`);
} finally {
  await rm(temporary, { recursive: true, force: true });
}
