import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, rmSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const root = resolve(desktop, "..");

export function buildOptions(
  argv: string[],
  host: string,
  targetDir: string,
  platform: NodeJS.Platform,
  environmentTarget?: string,
) {
  const [mode = "build", ...args] = argv;
  if (mode !== "build" && mode !== "dev") throw new Error("Use build or dev");
  const separator = args.indexOf("--");
  const options = separator < 0 ? args : args.slice(0, separator);
  const cargoArgs = separator < 0 ? [] : args.slice(separator + 1);
  const cliArgs: string[] = [];
  let requested: string | undefined;
  for (let index = 0; index < options.length; index++) {
    const arg = options[index];
    let value: string | undefined;
    if (arg === "--target" || arg === "-t") value = options[++index] ?? "";
    else if (arg.startsWith("--target=")) value = arg.slice(9);
    else if (arg.startsWith("-t") && !arg.startsWith("--"))
      value = arg.slice(2);
    else {
      cliArgs.push(arg);
      continue;
    }
    if (!value || value.startsWith("-") || requested !== undefined)
      throw new Error("Specify one --target <Rust target> before --");
    requested = value;
  }
  if (
    cargoArgs.some((arg) => arg === "--target" || arg.startsWith("--target="))
  )
    throw new Error("Pass --target before -- so the App and helper match");
  const explicitTarget = requested || environmentTarget;
  const target = explicitTarget || host;
  const supported =
    platform === "darwin"
      ? ["aarch64-apple-darwin", "x86_64-apple-darwin"]
      : platform === "win32"
        ? ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"]
        : [];
  if (!supported.includes(target))
    throw new Error(`Unsupported desktop target ${target} on ${platform}`);
  if (explicitTarget) cliArgs.push("--target", target);
  const debug =
    mode === "dev"
      ? !cliArgs.includes("--release")
      : cliArgs.includes("--debug");
  const profileDir = resolve(
    targetDir,
    ...(explicitTarget ? [target] : []),
    debug ? "debug" : "release",
  );
  return {
    mode,
    debug,
    target,
    targetArgs: explicitTarget ? ["--target", target] : [],
    profileDir,
    cliArgs,
    cargoArgs,
  };
}

function main() {
  const targetDir = resolve(root, process.env.CARGO_TARGET_DIR || "target");
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    CARGO_TARGET_DIR: targetDir,
  };
  if (process.platform === "darwin") env.MACOSX_DEPLOYMENT_TARGET = "13.0";
  function run(program: string, argv: string[], cwd = root) {
    const result = spawnSync(program, argv, { cwd, env, stdio: "inherit" });
    if (result.error) throw result.error;
    if (result.status !== 0) process.exit(result.status ?? 1);
  }
  const rustc = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
  if (rustc.error) throw rustc.error;
  const host = rustc.stdout.match(/^host: (.+)$/m)?.[1];
  if (rustc.status !== 0 || !host)
    throw new Error(`Cannot determine the native Rust target: ${rustc.stderr}`);
  const { mode, debug, target, targetArgs, profileDir, cliArgs, cargoArgs } =
    buildOptions(
      process.argv.slice(2),
      host,
      targetDir,
      process.platform,
      process.env.CARGO_BUILD_TARGET,
    );
  const extension = process.platform === "win32" ? ".exe" : "";
  run("cargo", [
    "build",
    "--locked",
    "-p",
    "xrun",
    ...targetArgs,
    ...(process.platform === "win32" ? ["--features", "desktop-helper"] : []),
    ...(debug ? [] : ["--release"]),
  ]);
  const binaries = resolve(desktop, "src-tauri/binaries");
  mkdirSync(binaries, { recursive: true });
  copyFileSync(
    resolve(profileDir, `xrun${extension}`),
    resolve(binaries, `xrun-${target}${extension}`),
  );
  if (mode === "build" && !cliArgs.includes("--no-bundle")) {
    // Cargo caches can restore installers from earlier versions alongside a new build.
    rmSync(resolve(profileDir, "bundle"), { recursive: true, force: true });
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
    !cliArgs.includes("--no-bundle")
  ) {
    const app = resolve(profileDir, "bundle/macos/xrun.app");
    if (existsSync(app)) {
      run("codesign", ["--verify", "--deep", "--strict", "--verbose=2", app]);
      run("ditto", ["-c", "-k", "--keepParent", app, `${app}.zip`]);
    }
  }
}

if (import.meta.main) main();
