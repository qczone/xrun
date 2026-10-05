import { chmod, mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { randomRoute } from "../src/auth";
export const root = resolve(import.meta.dir, "..");
export interface Deployment { name: string; route: string; url?: string; account?: string; removed?: boolean }
export function nameArgument(): string {
  const args = Bun.argv.slice(2);
  if (args.length > 2 || (args.length && (args[0] !== "--name" || !args[1]))) throw new Error("Usage: bun run deploy --name <worker-name>");
  const name = args[1] || "xrun-relay";
  if (!/^[a-z0-9][a-z0-9-]{0,62}$/.test(name)) throw new Error("Invalid Worker name");
  return name;
}
export function statePath(name: string): string { return resolve(root, ".deploy", `${name}.json`); }
export async function save(state: Deployment): Promise<void> {
  await mkdir(resolve(root, ".deploy"), { recursive: true, mode: 0o700 });
  await chmod(resolve(root, ".deploy"), 0o700);
  await writeFile(statePath(state.name), JSON.stringify(state, null, 2) + "\n", { mode: 0o600 });
  await chmod(statePath(state.name), 0o600);
}
export async function load(name: string, create: boolean): Promise<Deployment> {
  try {
    const state: Deployment = JSON.parse(await readFile(statePath(name), "utf8"));
    if (state.name !== name || !/^[a-z2-7]{26}$/.test(state.route) || state.removed) throw new Error("Invalid or removed deployment state; use a new Worker name");
    return state;
  } catch (error) {
    if (!create || (error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    const state = { name, route: randomRoute(), account: process.env.CLOUDFLARE_ACCOUNT_ID };
    await save(state);
    return state;
  }
}
export async function wrangler(args: string[], input?: string): Promise<string> {
  const child = Bun.spawn([process.execPath, "x", "--no-install", "wrangler", ...args], {
    cwd: root,
    env: { ...process.env, WRANGLER_SEND_METRICS: "false", WRANGLER_LOG_PATH: process.env.WRANGLER_LOG_PATH || resolve(root, ".wrangler", "logs", "wrangler.log") },
    stdin: input === undefined ? "ignore" : new TextEncoder().encode(input),
    stdout: "pipe", stderr: "inherit",
  });
  const output = await new Response(child.stdout).text();
  const code = await child.exited;
  if (code !== 0) throw new Error(`Wrangler failed (${code}): ${output}`);
  return output;
}
