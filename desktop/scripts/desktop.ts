import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, rmSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const root = resolve(desktop, "..");
const [mode = "build", ...args] = process.argv.slice(2);
if (mode !== "build" && mode !== "dev") throw new Error("Use build or dev");
const debug =
  mode === "dev" ? !args.includes("--release") : args.includes("--debug");
const targetDir = resolve(root, process.env.CARGO_TARGET_DIR || "target");
const env: NodeJS.ProcessEnv = { ...process.env, CARGO_TARGET_DIR: targetDir };
if (process.platform === "darwin") env.MACOSX_DEPLOYMENT_TARGET = "13.0";
function run(program: string, argv: string[], cwd = root) {
  const result = spawnSync(program, argv, { cwd, env, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}
const rustc = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
const target = rustc.stdout.match(/^host: (.+)$/m)?.[1];
if (!target) throw new Error("Cannot determine the native Rust target");
const extension = process.platform === "win32" ? ".exe" : "";
run("cargo", [
  "build",
  "--locked",
  "-p",
  "xrun",
  ...(process.platform === "win32" ? ["--features", "desktop-helper"] : []),
  ...(debug ? [] : ["--release"]),
]);
const binaries = resolve(desktop, "src-tauri/binaries");
mkdirSync(binaries, { recursive: true });
copyFileSync(
  resolve(targetDir, debug ? "debug" : "release", `xrun${extension}`),
  resolve(binaries, `xrun-${target}${extension}`),
);
const separator = args.indexOf("--");
const cliArgs = separator < 0 ? args : args.slice(0, separator);
const cargoArgs = separator < 0 ? [] : args.slice(separator + 1);
if (mode === "build" && !cliArgs.includes("--no-bundle")) {
  // Cargo caches can restore installers from earlier versions alongside a new build.
  rmSync(resolve(targetDir, debug ? "debug" : "release", "bundle"), {
    recursive: true,
    force: true,
  });
}
run(
  process.execPath,
  [
    resolve(desktop, "node_modules/@tauri-apps/cli/tauri.js"),
    mode,
    ...cliArgs,
    "--",
    "--locked",
    ...cargoArgs,
  ],
  desktop,
);
if (
  mode === "build" &&
  process.platform === "darwin" &&
  !args.includes("--no-bundle")
) {
  const app = resolve(
    targetDir,
    debug ? "debug" : "release",
    "bundle/macos/xrun.app",
  );
  if (existsSync(app))
    run("codesign", ["--verify", "--deep", "--strict", "--verbose=2", app]);
}
