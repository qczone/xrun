import {
  mkdtemp,
  mkdir,
  readFile,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";
import { randomRoute } from "../src/auth";
import { fixture, VERSION } from "../tests/fixtures";
import { Socket } from "../tests/socket";

// Isolate the forwarding loop from Rust/TLS overhead. The interoperability
// suite separately times actual 64 MiB push/pull using the production Worker.
const repository = resolve(import.meta.dir, "../..");
const baseline = process.argv[2];
if (!baseline)
  throw new Error(
    "Usage: bun cloudflare/scripts/benchmark-relay.ts <baseline git ref>",
  );
const temporary = await mkdtemp(join(tmpdir(), "xrun-relay-benchmark-"));
async function git(...args: string[]): Promise<string> {
  const process = Bun.spawn(["git", ...args], {
    cwd: repository,
    stdout: "pipe",
    stderr: "inherit",
  });
  const output = await new Response(process.stdout).text();
  if ((await process.exited) !== 0) throw new Error(`git ${args[0]} failed`);
  return output;
}
async function measure(entry: string, sessions: number) {
  const build = await Bun.build({
    entrypoints: [entry],
    target: "browser",
    external: ["cloudflare:workers"],
  });
  if (!build.success)
    throw new AggregateError(build.logs, "Benchmark Worker could not build");
  const secret = randomRoute();
  const runtime = new Miniflare(
    convertV4MiniflareOptions({
      modules: true,
      script: await build.outputs[0].text(),
      compatibilityDate: "2026-10-03",
      durableObjects: { NETWORKS: { className: "TestRelay", useSQLite: true } },
      bindings: {
        RELAY_ROUTE: secret,
        XRUN_VERSION: VERSION,
        XRUN_TEST_PROOF_GATE: "http://127.0.0.1",
      },
    }),
  );
  try {
    const origin = (await runtime.ready).origin;
    const network = await fixture(),
      target = await network.member(),
      source = await network.member();
    async function open(path: string) {
      const response = await runtime.dispatchFetch(
        `${origin}/${secret}${path}`,
        {
          headers: { Upgrade: "websocket", "X-Xrun-Version": VERSION },
        },
      );
      if (!response.webSocket) throw new Error(`Upgrade ${response.status}`);
      return new Socket(response.webSocket);
    }
    async function authenticate(
      path: string,
      member: Awaited<ReturnType<typeof network.member>>,
    ) {
      const socket = await open(path);
      const challenge = await socket.json();
      socket.send({
        type: "authenticate",
        proof: await member.proof(path, challenge.nonce),
      });
      return socket;
    }
    const controlPath = `/networks/${network.network}/control`;
    const control = await authenticate(controlPath, target);
    const hello = await control.json();
    // Other online devices make a scan's cost visible without consuming sessions.
    const controls = [control];
    for (let index = 1; index < 32; index++) {
      const member = await network.member();
      const device = await authenticate(controlPath, member);
      await device.json();
      controls.push(device);
    }
    const pairs: { sender: Socket; receiver: Socket }[] = [];
    for (let index = 0; index < sessions; index++) {
      const path = `/networks/${network.network}/connect/${target.device}`;
      const sender = await authenticate(path, source);
      const incoming = await control.json();
      const receiver = await open(
        `/networks/${network.network}/attach/${target.device}/${hello.generation}/${incoming.session_id}`,
      );
      await sender.json();
      await receiver.json();
      pairs.push({ sender, receiver });
    }
    const namespace = await runtime.getDurableObjectNamespace("NETWORKS");
    const object = namespace.get(namespace.idFromName(network.network));
    const metrics = async () =>
      (await object.fetch("https://test/test/metrics")).json() as Promise<{
        enumeration: number;
        alarm: number;
      }>;
    const before = await metrics();
    const frame = new Uint8Array(64 * 1024).buffer;
    const started = performance.now();
    for (const reverse of [false, true])
      for (let index = 0; index < 1024; index++) {
        const pair = pairs[index % pairs.length];
        const sender = reverse ? pair.receiver : pair.sender;
        const receiver = reverse ? pair.sender : pair.receiver;
        sender.send(frame);
        const received = await receiver.next();
        if (
          !(received instanceof ArrayBuffer) ||
          received.byteLength !== frame.byteLength
        )
          throw new Error("Frame changed");
        receiver.send({ type: "ack", bytes: frame.byteLength });
        const ack = await sender.json();
        if (ack.type !== "ack" || ack.bytes !== frame.byteLength)
          throw new Error("Credit changed");
      }
    const elapsedMs = performance.now() - started;
    const after = await metrics();
    for (const pair of pairs) {
      pair.sender.close();
      pair.receiver.close();
    }
    for (const device of controls) device.close();
    return {
      sessions,
      controls: controls.length,
      bytesEachDirection: 64 * 1024 * 1024,
      elapsedMs,
      socketEnumerations: after.enumeration - before.enumeration,
      alarmReads: after.alarm - before.alarm,
    };
  } finally {
    await runtime.dispose();
  }
}
try {
  const files = (
    await git("ls-tree", "-r", "--name-only", baseline, "cloudflare/src")
  )
    .trim()
    .split("\n");
  const sourceDirectory = join(temporary, "src");
  for (const file of files) {
    const destination = join(
      sourceDirectory,
      file.slice("cloudflare/src/".length),
    );
    await mkdir(resolve(destination, ".."), { recursive: true });
    await writeFile(destination, await git("show", `${baseline}:${file}`));
  }
  await symlink(
    resolve(import.meta.dir, "../node_modules"),
    join(temporary, "node_modules"),
    "junction",
  );
  const currentEntry = resolve(
    import.meta.dir,
    "../tests/workerd/relay-worker.ts",
  );
  const oldEntry = join(temporary, "worker.ts");
  const source = (await readFile(currentEntry, "utf8")).replace(
    'from "../../src/index"',
    `from ${JSON.stringify(join(sourceDirectory, "index.ts"))}`,
  );
  await writeFile(oldEntry, source);
  const results = [];
  for (const sessions of [1, 8]) {
    const before = await measure(oldEntry, sessions),
      after = await measure(currentEntry, sessions);
    results.push({
      baseline,
      before,
      after,
      speedup: before.elapsedMs / after.elapsedMs,
    });
  }
  console.log(
    JSON.stringify(
      {
        environment: {
          platform: process.platform,
          arch: process.arch,
          bun: Bun.version,
        },
        results,
      },
      null,
      2,
    ),
  );
} finally {
  await rm(temporary, { recursive: true, force: true });
}
