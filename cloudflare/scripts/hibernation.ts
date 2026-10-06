import { mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fixture, VERSION } from "../tests/fixtures";
import { RelaySocket } from "./relay-socket";

// Deploy tests/workerd/deployed-probe.ts to a temporary Worker first; the link
// stays in a private file. No test route is present in the production bundle.
const file = process.env.XRUN_TEST_CF_LINK_FILE;
if (!file)
  throw new Error("Set XRUN_TEST_CF_LINK_FILE for a temporary probe Worker");
const link = new URL((await readFile(file, "utf8")).trim());
const prefix = link.pathname.replace(/\/$/, "");
const network = await fixture();
const target = await network.member(),
  source = await network.member();
const clients: RelaySocket[] = [];
function check(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}
async function open(path: string): Promise<RelaySocket> {
  const socket = new RelaySocket(`wss://${link.host}${prefix}${path}`, VERSION);
  clients.push(socket);
  await socket.ready;
  return socket;
}
async function authenticated(
  path: string,
  member: Awaited<ReturnType<typeof network.member>>,
) {
  const socket = await open(path);
  const challenge = await socket.json();
  check(challenge.type === "challenge", "missing challenge");
  socket.send({
    type: "authenticate",
    proof: await member.proof(path, challenge.nonce),
  });
  return socket;
}
interface Snapshot {
  instance: string;
  sockets: { role: string; outstanding?: number; deadline?: number }[];
}
async function inspect(): Promise<Snapshot> {
  const response = await fetch(
    `https://${link.host}${prefix}/probe/${network.network}`,
  );
  check(response.ok, `probe returned ${response.status}`);
  return response.json() as Promise<Snapshot>;
}
try {
  const control = await authenticated(
    `/networks/${network.network}/control`,
    target,
  );
  const hello = await control.json();
  const sender = await authenticated(
    `/networks/${network.network}/connect/${target.device}`,
    source,
  );
  const incoming = await control.json();
  const receiver = await open(
    `/networks/${network.network}/attach/${target.device}/${hello.generation}/${incoming.session_id}`,
  );
  check((await sender.json()).type === "connected", "sender was not connected");
  check(
    (await receiver.json()).type === "connected",
    "receiver was not connected",
  );
  const frame = new Uint8Array(64 * 1024).buffer;
  sender.send(frame);
  check(
    ((await receiver.next()) as ArrayBuffer).byteLength === frame.byteLength,
    "frame mismatch",
  );
  const before = await inspect();
  check(
    before.sockets.find((state) => state.role === "source")?.outstanding ===
      frame.byteLength,
    "initial credit missing",
  );
  let after = before;
  let idleSeconds = 0;
  for (
    let attempt = 0;
    attempt < 4 && after.instance === before.instance;
    attempt++
  ) {
    console.log(
      "Waiting 30 seconds without WebSocket messages or probe requests…",
    );
    await Bun.sleep(30000);
    idleSeconds += 30;
    after = await inspect();
  }
  check(
    after.instance !== before.instance,
    "no object reconstruction observed; hibernation is unverified",
  );
  const old = before.sockets.find((state) => state.role === "source");
  const restored = after.sockets.find((state) => state.role === "source");
  check(
    restored?.outstanding === frame.byteLength &&
      restored.deadline === old?.deadline,
    "reconstruction changed credit or idle deadline",
  );
  receiver.send({ type: "ack", bytes: frame.byteLength });
  check(
    (await sender.json()).bytes === frame.byteLength,
    "restored credit acknowledgement failed",
  );
  // Exact 4 MiB window, then one byte beyond it, after object reconstruction.
  for (let index = 0; index < 64; index++) {
    sender.send(frame);
    check(
      ((await receiver.next()) as ArrayBuffer).byteLength === frame.byteLength,
      "restored tunnel failed",
    );
  }
  sender.send(new Uint8Array([1]).buffer);
  check(
    (await sender.json()).code === "MESSAGE_TOO_LARGE",
    "restored flow-control window was bypassed",
  );
  await receiver.next().then(
    () => {
      throw new Error("peer remained open");
    },
    (error) => {
      check(
        error instanceof Error && error.message === "closed",
        "peer did not close",
      );
    },
  );
  const output =
    process.env.XRUN_HIBERNATION_OUTPUT ||
    resolve("../output/evolution-20261007/hibernation.json");
  await mkdir(resolve(output, ".."), { recursive: true });
  await writeFile(
    output,
    JSON.stringify(
      {
        date: new Date().toISOString(),
        idle_seconds: idleSeconds,
        instance_changed: true,
        deadline_preserved: true,
        credit_preserved: true,
        window_enforced: true,
        peer_closed: true,
        before,
        after,
      },
      null,
      2,
    ) + "\n",
  );
  console.log(`Real object reconstruction verified; evidence: ${output}`);
} finally {
  for (const socket of clients) socket.close();
}
