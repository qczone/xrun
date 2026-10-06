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

// Each ref uses its own test harness and wire contract in separate temporary homes.
// CPU timings never run under syscall tracing; output chunks are not fsync counts.
const root = resolve(import.meta.dir, "../..");
const baseline = process.argv[2];
if (!baseline)
  throw new Error(
    "Usage: bun desktop/scripts/benchmark-core.ts <baseline git ref>",
  );
const temporary = await mkdtemp(join(tmpdir(), "xrun-core-benchmark-"));
const targetDirectory = resolve(root, process.env.CARGO_TARGET_DIR || "target");
const tracing = process.env.XRUN_BENCH_TRACE === "1";
if (tracing && process.platform !== "linux")
  throw new Error("Syscall measurements require Linux strace");
const environment = {
  ...process.env,
  CARGO_TARGET_DIR: targetDirectory,
  CARGO_PROFILE_DEV_DEBUG: "0",
  CARGO_PROFILE_TEST_DEBUG: "0",
  CARGO_INCREMENTAL: "0",
};
async function run(args: string[], cwd = root): Promise<string> {
  const child = Bun.spawn(args, {
    cwd,
    env: environment,
    stdout: "pipe",
    stderr: "inherit",
  });
  const output = await new Response(child.stdout).text();
  if (await child.exited) throw new Error(`${args[0]} ${args[1]} failed`);
  return output;
}
async function build(checkout: string, binaryName: string, label: string) {
  const result = await run(
    [
      "cargo",
      "test",
      "--locked",
      "-p",
      "xrun",
      "--test",
      "performance",
      "--no-run",
      "--message-format=json-render-diagnostics",
    ],
    checkout,
  );
  const artifacts = result
    .trim()
    .split("\n")
    .map(
      (line) =>
        JSON.parse(line) as {
          reason: string;
          target?: { name: string };
          executable?: string;
        },
    );
  const test = artifacts.find(
    (item) =>
      item.reason === "compiler-artifact" &&
      item.target?.name === "performance" &&
      item.executable,
  )?.executable;
  if (!test)
    throw new Error("Cargo did not report the benchmark test executable");
  const suffix = process.platform === "win32" ? ".exe" : "";
  const binary = join(temporary, `${label}-cli${suffix}`),
    harness = join(temporary, `${label}-test${suffix}`);
  await copyFile(join(targetDirectory, `debug/${binaryName}${suffix}`), binary);
  await copyFile(test, harness);
  await chmod(binary, 0o700);
  await chmod(harness, 0o700);
  return {
    binary,
    harness,
    checkout,
    version: (await run([binary, "--version"])).trim(),
  };
}
interface Sample {
  sample_started_ms: number;
  sample_ended_ms: number;
}
interface CoreMeasurement {
  idle?: Record<string, Sample> | null;
  logs: { runs: Sample[] };
}
function sampleWindows(measurement: CoreMeasurement) {
  const samples = [
    ...Object.entries(measurement.idle || {}),
    ...measurement.logs.runs.map((value, index): [string, Sample] => [
      `logs_${index + 1}`,
      value,
    ]),
  ];
  return samples.map(([name, value]) => {
    if (!value.sample_started_ms || !value.sample_ended_ms)
      throw new Error("Syscall sample has no wall-clock boundaries");
    return {
      name,
      start_ms: value.sample_started_ms,
      end_ms: value.sample_ended_ms,
      configuration_open_calls: 0,
      configuration_read_calls: 0,
      task_database_sync_calls: 0,
    };
  });
}
async function syscallCounts(path: string, measurement: CoreMeasurement) {
  const samples = sampleWindows(measurement);
  for (const line of (await readFile(path, "utf8")).split("\n")) {
    const match =
      /^\s*\d+\s+(\d+\.\d+)\s+(read|pread64|openat|fsync|fdatasync)\(/.exec(
        line,
      );
    if (!match) continue;
    const timestamp = Number(match[1]) * 1000;
    const configuration =
      /\/target\/\.xrun\/(daemon\.toml|identity\.toml|roster\.db)/.test(line);
    const taskDatabase = line.includes("/target/.xrun/daemon.db");
    for (const sample of samples) {
      if (timestamp < sample.start_ms || timestamp > sample.end_ms) continue;
      if (configuration && match[2] === "openat")
        sample.configuration_open_calls++;
      if (configuration && ["read", "pread64"].includes(match[2]))
        sample.configuration_read_calls++;
      if (taskDatabase && ["fsync", "fdatasync"].includes(match[2]))
        sample.task_database_sync_calls++;
    }
  }
  return {
    scope:
      "Target daemon only; config, identity and roster file reads; task database and WAL sync calls include job-state commits",
    strings_suppressed: true,
    samples,
  };
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
  const extract = Bun.spawn(["tar", "-x", "-C", checkout], {
    stdin: archive.stdout,
    stdout: "inherit",
    stderr: "inherit",
  });
  if ((await extract.exited) || (await archive.exited))
    throw new Error("Could not extract baseline source");
  let harnessReference = reference;
  if (!(await Bun.file(join(checkout, "tests/performance.rs")).exists())) {
    // The original performance baseline predates this measurement fixture.
    // Compile the first fixture against that ref's own library and CLI.
    harnessReference = (
      await run(["git", "rev-parse", "77ffcd1^{commit}"])
    ).trim();
    for (const file of [
      "tests/performance.rs",
      "tests/fixtures/log-producer.rs",
    ]) {
      const destination = join(checkout, file);
      await mkdir(resolve(destination, ".."), { recursive: true });
      await writeFile(
        destination,
        await run(["git", "show", `${harnessReference}:${file}`]),
      );
    }
  }
  const manifest = join(checkout, "Cargo.toml");
  await writeFile(
    manifest,
    (await readFile(manifest, "utf8")).replace(
      "[package]",
      "[package]\nautobins = false",
    ) +
      '\n[lib]\nname = "xrun_benchmark_baseline"\n\n[[bin]]\nname = "xrun-benchmark-baseline"\npath = "src/main.rs"\n',
  );
  for (const file of [
    "src/main.rs",
    "tests/common/mod.rs",
    "tests/performance.rs",
  ]) {
    const path = join(checkout, file);
    let source = await readFile(path, "utf8");
    if (
      file === "tests/performance.rs" &&
      !source.includes("sample_started_ms")
    ) {
      // Add measurement boundaries only; baseline production code is untouched.
      source = source
        .replace(
          "let before = usage(pid)?;",
          "let before = usage(pid)?;\n    let sample_started_ms = xrun::protocol::now_ms();",
        )
        .replace(
          '"sample_seconds": elapsed,',
          '"sample_seconds": elapsed,\n        "sample_started_ms":sample_started_ms,"sample_ended_ms":xrun::protocol::now_ms(),',
        )
        .replace(
          "for _ in 0..3 {\n        let started = Instant::now();",
          "for _ in 0..3 {\n        let sample_started_ms = xrun::protocol::now_ms();\n        let started = Instant::now();",
        )
        .replace(
          '"elapsed_seconds":elapsed,"persisted_chunks":chunks,',
          '"sample_started_ms":sample_started_ms,"sample_ended_ms":xrun::protocol::now_ms(),\n            "elapsed_seconds":elapsed,"persisted_chunks":chunks,',
        );
    }
    await writeFile(
      path,
      source
        .replaceAll("xrun::", "xrun_benchmark_baseline::")
        .replaceAll(
          "CARGO_BIN_EXE_xrun",
          "CARGO_BIN_EXE_xrun-benchmark-baseline",
        ),
    );
  }
  const before = await build(checkout, "xrun-benchmark-baseline", "baseline");
  const after = await build(root, "xrun", "current");
  const measurements: Record<string, unknown> = {};
  for (const [label, snapshot] of [
    ["baseline", before],
    ["current", after],
  ] as const) {
    const output = join(temporary, `${label}.json`);
    const traceFile = join(temporary, `${label}.strace`);
    const command = [
      snapshot.harness,
      "--ignored",
      "--nocapture",
      "--test-threads=1",
    ];
    if (tracing)
      command.unshift(
        "strace",
        "-f",
        "-qq",
        "-ttt",
        "-yy",
        "-s",
        "0",
        "-e",
        "trace=read,pread64,openat,fsync,fdatasync",
        "-o",
        traceFile,
      );
    const child = Bun.spawn(command, {
      cwd: snapshot.checkout,
      env: {
        ...environment,
        XRUN_TEST_BINARY: snapshot.binary,
        XRUN_BENCH_OUTPUT: output,
      },
      stdout: "inherit",
      stderr: "inherit",
    });
    if (await child.exited) throw new Error(`${label} measurement failed`);
    const measurement = JSON.parse(await readFile(output, "utf8"));
    measurements[label] = {
      version: snapshot.version,
      binary_sha256: new Bun.CryptoHasher("sha256")
        .update(await Bun.file(snapshot.binary).arrayBuffer())
        .digest("hex"),
      ...measurement,
      ...(tracing
        ? { syscall_counts: await syscallCounts(traceFile, measurement) }
        : {}),
    };
  }
  const outputDirectory = resolve(
    root,
    process.env.XRUN_BENCH_OUTPUT_DIR || "output/benchmarks",
  );
  await mkdir(outputDirectory, { recursive: true });
  const output = join(
    outputDirectory,
    `core-${process.env.XRUN_BENCH_LOG_MODE || "dense"}-${tracing ? "syscalls-" : ""}${process.platform}.json`,
  );
  await writeFile(
    output,
    JSON.stringify(
      {
        baseline_ref: reference,
        baseline_harness_ref: harnessReference,
        current_ref: (await run(["git", "rev-parse", "HEAD"])).trim(),
        profile:
          "unoptimized dev, debug symbols disabled, incremental disabled",
        timings_affected_by_tracing: tracing,
        ...measurements,
      },
      null,
      2,
    ) + "\n",
  );
  console.log(`Core benchmark written to ${output}`);
} finally {
  await rm(temporary, { recursive: true, force: true });
}
