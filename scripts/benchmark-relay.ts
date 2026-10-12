/** Compare owned localhost WSS forwarding on the actual filesystem; no external relay is contacted. */
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir, cpus, totalmem } from "node:os";
import { join, resolve } from "node:path";

const repository = resolve(import.meta.dir, "..");
const baseline = process.argv[2] ?? process.env.XRUN_BASELINE_REF ?? "34411cc";
const temporary = await mkdtemp(join(tmpdir(), "xrun-disk-relay-"));
async function run(
  args: string[],
  cwd: string,
  env: Record<string, string | undefined> = {},
) {
  const child = Bun.spawn(args, {
    cwd,
    env: { ...process.env, ...env },
    stdout: "pipe",
    stderr: "inherit",
  });
  const output = await new Response(child.stdout).text();
  if ((await child.exited) !== 0)
    throw new Error(`${args[0]} ${args[1]} failed`);
  return output;
}
async function measure(cwd: string, target: string) {
  const samples = [];
  for (let repeat = 0; repeat < 3; repeat++) {
    const output = await run(
      [
        "cargo",
        "test",
        "--test",
        "relay",
        "relay_disk_throughput",
        "--",
        "--ignored",
        "--nocapture",
      ],
      cwd,
      { CARGO_TARGET_DIR: target },
    );
    const record = output
      .split("\n")
      .find((line) => line.startsWith("relay_disk_benchmark "));
    if (!record) throw new Error("Benchmark did not produce a measurement");
    samples.push(JSON.parse(record.slice("relay_disk_benchmark ".length)));
  }
  return samples;
}
try {
  const checkout = join(temporary, "before");
  await mkdir(checkout);
  await run(
    [
      "git",
      "archive",
      "--format=tar",
      `--output=${join(temporary, "before.tar")}`,
      baseline,
    ],
    repository,
  );
  await run(
    ["tar", "-xf", join(temporary, "before.tar"), "-C", checkout],
    repository,
  );
  const currentTests = await readFile(
    join(repository, "tests/relay.rs"),
    "utf8",
  );
  const marker =
    '#[tokio::test(flavor = "multi_thread", worker_threads = 4)]\n#[ignore = "owned localhost throughput';
  const start = currentTests.indexOf(marker);
  if (start < 0) throw new Error("Relay benchmark fixture missing");
  const path = join(checkout, "tests/relay.rs");
  await writeFile(
    path,
    (await readFile(path, "utf8")) + "\n" + currentTests.slice(start),
  );
  const output = resolve(
    process.env.XRUN_BENCH_OUTPUT_DIR || join(repository, "output/benchmarks"),
  );
  await mkdir(output, { recursive: true });
  const before = await measure(
    checkout,
    process.env.XRUN_BENCH_BASELINE_TARGET ||
      join(repository, "target/relay-benchmark-before"),
  );
  const after = await measure(repository, join(repository, "target"));
  const results = {
    baseline,
    afterRef: "working-tree",
    scope:
      "localhost WSS opaque frames, native SQLite on the real filesystem; no inner TLS",
    environment: {
      platform: process.platform,
      arch: process.arch,
      cpu: cpus()[0]?.model,
      memoryBytes: totalmem(),
      bun: Bun.version,
    },
    before,
    after,
  };
  const pathOut = join(output, `relay-native-${process.platform}.json`);
  await writeFile(pathOut, JSON.stringify(results, null, 2) + "\n");
  console.info(JSON.stringify(results, null, 2));
  console.info(`Saved ${pathOut}`);
} finally {
  await rm(temporary, { recursive: true, force: true });
}
