import {
  chmod,
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// Real historical source builds. Old release-bound beta protocols are intentionally excluded.
const root = resolve(import.meta.dir, "..");
const targetDirectory = resolve(root, process.env.CARGO_TARGET_DIR || "target");
const environment = {
  ...process.env,
  CARGO_TARGET_DIR: targetDirectory,
  CARGO_PROFILE_DEV_DEBUG: "0",
};
async function run(
  args: string[],
  cwd = root,
  env = environment,
): Promise<string> {
  const child = Bun.spawn(args, {
    cwd,
    env,
    stdout: "pipe",
    stderr: "inherit",
  });
  const output = await new Response(child.stdout).text();
  if (await child.exited) throw new Error(`${args[0]} ${args[1]} failed`);
  return output;
}
function range(source: string) {
  const max = Number(/pub const PROTOCOL: u32 = (\d+);/.exec(source)?.[1]);
  const min = Number(/min:\s*(\d+)/.exec(source)?.[1]);
  if (!min || !max || min > max)
    throw new Error("No implemented protocol range");
  return { min, max };
}
const currentVersion = /^version = "([^"]+)"/m.exec(
  await readFile(join(root, "Cargo.toml"), "utf8"),
)?.[1];
const current = range(
  await readFile(join(root, "src/protocol/version.rs"), "utf8"),
);
let baseline = process.argv[2];
if (!baseline) {
  const tags = (await run(["git", "tag", "--sort=-version:refname"]))
    .trim()
    .split("\n")
    .filter(Boolean);
  for (const tag of tags) {
    try {
      const manifest = await run(["git", "show", `${tag}:Cargo.toml`]);
      if (/^version = "([^"]+)"/m.exec(manifest)?.[1] === currentVersion)
        continue;
      const previous = range(
        await run(["git", "show", `${tag}:src/protocol/version.rs`]),
      );
      if (
        Math.max(current.min, previous.min) <=
        Math.min(current.max, previous.max)
      ) {
        baseline = tag;
        break;
      }
    } catch {
      /* Before protocol 1 there is no supported historical compatibility. */
    }
  }
}
if (!baseline) {
  console.log(
    "Protocol 1 bootstrap: no earlier compatible release tag exists; future releases test real previous builds.",
  );
} else {
  const reference = (
    await run(["git", "rev-parse", "--verify", `${baseline}^{commit}`])
  ).trim();
  const previous = range(
    await run(["git", "show", `${reference}:src/protocol/version.rs`]),
  );
  if (Math.max(current.min, previous.min) > Math.min(current.max, previous.max))
    throw new Error("Selected historical build has no common protocol");
  const temporary = await mkdtemp(join(tmpdir(), "xrun-compatibility-"));
  try {
    const checkout = join(temporary, "previous");
    await mkdir(checkout);
    const archive = Bun.spawn(["git", "archive", reference], {
      cwd: root,
      stdout: "pipe",
      stderr: "inherit",
    });
    const extract = Bun.spawn(["tar", "-x"], {
      cwd: checkout,
      stdin: archive.stdout,
      stdout: "inherit",
      stderr: "inherit",
    });
    if ((await extract.exited) || (await archive.exited))
      throw new Error("Could not extract historical source");
    const manifest = join(checkout, "Cargo.toml");
    await writeFile(
      manifest,
      (await readFile(manifest, "utf8")).replace(
        "[package]",
        "[package]\nautobins = false",
      ) +
        '\n[lib]\nname = "xrun_compatibility_previous"\n\n[[bin]]\nname = "xrun-compatibility-previous"\npath = "src/main.rs"\n',
    );
    const entry = join(checkout, "src/main.rs");
    await writeFile(
      entry,
      (await readFile(entry, "utf8")).replaceAll(
        "xrun::",
        "xrun_compatibility_previous::",
      ),
    );
    await run(
      [
        "cargo",
        "build",
        "--locked",
        "-p",
        "xrun",
        "--bin",
        "xrun-compatibility-previous",
      ],
      checkout,
    );
    const suffix = process.platform === "win32" ? ".exe" : "";
    const oldBinary = join(temporary, `previous-cli${suffix}`);
    await copyFile(
      join(targetDirectory, `debug/xrun-compatibility-previous${suffix}`),
      oldBinary,
    );
    await chmod(oldBinary, 0o700);
    const output = await run([
      "cargo",
      "test",
      "--locked",
      "--test",
      "cloudflare",
      "--no-run",
      "--message-format=json-render-diagnostics",
    ]);
    const artifacts = output
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
        item.target?.name === "cloudflare" &&
        item.executable,
    )?.executable;
    if (!test) throw new Error("No compatibility test executable");
    const testSnapshot = join(temporary, `tests${suffix}`),
      currentBinary = join(temporary, `current${suffix}`);
    await copyFile(test, testSnapshot);
    await copyFile(join(targetDirectory, `debug/xrun${suffix}`), currentBinary);
    await chmod(testSnapshot, 0o700);
    await chmod(currentBinary, 0o700);
    const env = {
      ...environment,
      XRUN_TEST_BINARY: currentBinary,
      XRUN_TEST_PREVIOUS_BINARY: oldBinary,
    };
    if (process.platform === "linux") {
      for (const side of ["source", "target", "relay"]) {
        console.log(
          `Rust relay compatibility against ${baseline}: previous ${side}`,
        );
        await run(
          [testSnapshot, "--ignored", "--nocapture", "--test-threads=1"],
          root,
          {
            ...env,
            XRUN_TEST_PREVIOUS_SIDE: side,
            XRUN_TEST_RELAY_KIND: "rust",
          },
        );
      }
    }
    await symlink(
      join(root, "cloudflare/node_modules"),
      join(checkout, "cloudflare/node_modules"),
      "junction",
    );
    await run(["bun", "scripts/build.ts"], join(checkout, "cloudflare"));
    await run(["bun", "scripts/build.ts"], join(root, "cloudflare"));
    for (const oldWorker of [false, true]) {
      for (const side of ["source", "target"]) {
        console.log(
          `Cloudflare compatibility against ${baseline}: previous ${side}, ${oldWorker ? "previous" : "current"} Worker`,
        );
        await run(["bun", "scripts/interop.ts"], join(root, "cloudflare"), {
          ...env,
          XRUN_TEST_PREVIOUS_SIDE: side,
          ...(oldWorker
            ? {
                XRUN_TEST_CF_WORKER: join(
                  checkout,
                  "cloudflare/.wrangler/build/index.js",
                ),
                XRUN_TEST_CF_PROTOCOL: `${previous.min}-${previous.max}`,
              }
            : {}),
        });
      }
    }
    console.log(
      `Mixed-version joining, execution, transfers, roster sync and revocation passed against ${baseline}.`,
    );
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}
