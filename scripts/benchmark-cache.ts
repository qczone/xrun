import {
  chmod,
  copyFile,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// Actual wall-clock gaps, independent target daemons and observable cache decisions.
// Every binary and home is isolated; the private relay URL is never written to the report.
const supplied = process.env.XRUN_TEST_BINARY;
const linkFile = process.env.XRUN_TEST_CF_LINK_FILE;
const output = process.env.XRUN_BENCH_CACHE_OUTPUT;
if (!supplied || !linkFile || !output)
  throw new Error(
    "Set XRUN_TEST_BINARY, XRUN_TEST_CF_LINK_FILE and XRUN_BENCH_CACHE_OUTPUT",
  );
const intervals = (process.env.XRUN_BENCH_CACHE_INTERVALS || "10,30,60")
  .split(",")
  .map(Number);
const rounds = Number(process.env.XRUN_BENCH_CACHE_ROUNDS || "3");
const targets = Number(process.env.XRUN_BENCH_CACHE_TARGETS || "3");
if (
  !intervals.every((value) => Number.isFinite(value) && value > 0) ||
  !Number.isInteger(rounds) ||
  rounds < 2 ||
  !Number.isInteger(targets) ||
  targets < 1 ||
  targets > 6
)
  throw new Error("Invalid interval, round or target count");
const directory = await mkdtemp(join(tmpdir(), "xrun-cache-benchmark-"));
const binary = join(
  directory,
  process.platform === "win32" ? "xrun.exe" : "xrun",
);
await copyFile(resolve(supplied), binary);
await chmod(binary, 0o700);
const link = (await readFile(linkFile, "utf8")).trim();
const source = join(directory, "source");
const homes = [
  source,
  ...Array.from({ length: targets }, (_, index) =>
    join(directory, `target${index + 1}`),
  ),
];
const daemons = new Map<string, ReturnType<typeof Bun.spawn>>();
function environment(home: string) {
  return {
    ...process.env,
    HOME: home,
    USERPROFILE: home,
    RUST_LOG: "xrun::pool=debug",
    NO_COLOR: "1",
  };
}
async function cli(home: string, args: string[]) {
  const child = Bun.spawn([binary, ...args], {
    env: environment(home),
    stdout: "pipe",
    stderr: "pipe",
  });
  const timer = setTimeout(() => child.kill(), 60_000);
  try {
    const [stdout, stderr, code] = await Promise.all([
      new Response(child.stdout).text(),
      new Response(child.stderr).text(),
      child.exited,
    ]);
    if (code) throw new Error(`CLI ${args[0]} failed (${code}): ${stderr}`);
    return stdout;
  } finally {
    clearTimeout(timer);
  }
}
function start(home: string) {
  const log = join(home, "daemon.log");
  const child = Bun.spawn([binary, "daemon"], {
    env: environment(home),
    stdout: Bun.file(log),
    stderr: Bun.file(log),
  });
  daemons.set(home, child);
}
async function stop(home: string) {
  const child = daemons.get(home);
  if (!child) return;
  try {
    await cli(home, ["daemon", "stop"]);
  } finally {
    const timer = setTimeout(() => child.kill(), 5000);
    await child.exited;
    clearTimeout(timer);
    daemons.delete(home);
  }
}
async function online(name: string) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    try {
      if (JSON.parse(await cli(source, [name, "info", "--json"])).online)
        return;
    } catch {
      /* Connecting. */
    }
    await Bun.sleep(250);
  }
  throw new Error(`Device ${name} did not connect`);
}
const terminate = () => {
  for (const child of daemons.values()) child.kill();
};
process.on("SIGINT", terminate);
process.on("SIGTERM", terminate);
try {
  const version = (await cli(source, ["--version"])).trim();
  await cli(source, [
    "up",
    "--relay",
    link,
    "--name",
    "source1",
    "--no-daemon",
  ]);
  start(source);
  await online("source1");
  for (let index = 0; index < targets; index++) {
    const invitation = JSON.parse(
      await cli(source, ["invite", "--allow", "--json"]),
    );
    await cli(homes[index + 1], [
      "join",
      invitation.link,
      "--name",
      `target${index + 1}`,
      "--no-daemon",
    ]);
    start(homes[index + 1]);
    await online(`target${index + 1}`);
  }
  const results = [];
  for (const interval of intervals) {
    await stop(source);
    await writeFile(join(source, "daemon.log"), "");
    start(source);
    await online("source1");
    const observations = [];
    const startTime = performance.now();
    for (let round = 0; round < rounds; round++) {
      await Bun.sleep(
        Math.max(0, startTime + round * interval * 1000 - performance.now()),
      );
      for (let index = 1; index <= targets; index++) {
        const logBefore = await readFile(join(source, "daemon.log"), "utf8");
        const started = performance.now();
        const value = (
          await cli(source, [`target${index}`, "--", binary, "--version"])
        ).trim();
        if (value !== version) throw new Error("Unexpected command output");
        const latency = performance.now() - started;
        const added = (
          await readFile(join(source, "daemon.log"), "utf8")
        ).slice(logBefore.length);
        const hits = (added.match(/reusing operation session/g) || []).length;
        const opened = (added.match(/opened operation session/g) || []).length;
        if (hits + opened !== 1)
          throw new Error("Expected one observable operation session decision");
        observations.push({
          round,
          target: index,
          elapsed_ms: started - startTime,
          latency_ms: latency,
          hit: hits === 1,
          diagnostics: added
            .split("\n")
            .filter((line) =>
              /discarding cached|probe failed|not cached/.test(line),
            ),
        });
      }
      console.log(
        `${interval}s interval, round ${round + 1}/${rounds}, ${targets} targets complete`,
      );
    }
    results.push({
      interval_seconds: interval,
      targets,
      rounds,
      requests: observations.length,
      hits: observations.filter((value) => value.hit).length,
      observations,
    });
  }
  await writeFile(
    output,
    JSON.stringify(
      {
        platform: process.platform,
        arch: process.arch,
        version,
        binary_sha256: new Bun.CryptoHasher("sha256")
          .update(await Bun.file(binary).arrayBuffer())
          .digest("hex"),
        path: "public Cloudflare, isolated source and targets on the same host; real wall-clock intervals",
        results,
      },
      null,
      2,
    ) + "\n",
  );
  console.log(`Cache report written to ${output}`);
} finally {
  for (const home of homes)
    await stop(home).catch(() => daemons.get(home)?.kill());
  process.off("SIGINT", terminate);
  process.off("SIGTERM", terminate);
  await rm(directory, { recursive: true, force: true });
}
