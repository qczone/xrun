import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const root = resolve(desktop, "..");
const args = process.argv.slice(2);
const debug = args.includes("--debug");
const env = { ...process.env, CARGO_TARGET_DIR: resolve(root, process.env.CARGO_TARGET_DIR || "target") };
if (process.platform === "darwin") env.MACOSX_DEPLOYMENT_TARGET = "13.0";
function run(program, argv, cwd = root) {
  const result = spawnSync(program, argv, { cwd, env, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}
const rustc = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
const target = rustc.stdout.match(/^host: (.+)$/m)?.[1];
if (!target) throw new Error("Cannot determine the native Rust target");
const extension = process.platform === "win32" ? ".exe" : "";
run("cargo", ["build", "--locked", "-p", "xrun", ...(process.platform === "win32" ? ["--features", "desktop-helper"] : []), ...(debug ? [] : ["--release"])]);
const binaries = resolve(desktop, "src-tauri/binaries");
mkdirSync(binaries, { recursive: true });
copyFileSync(resolve(env.CARGO_TARGET_DIR, debug ? "debug" : "release", `xrun${extension}`), resolve(binaries, `xrun-${target}${extension}`));
run(process.execPath, [resolve(desktop, "node_modules/@tauri-apps/cli/tauri.js"), "build", ...args, "--", "--locked"], desktop);
