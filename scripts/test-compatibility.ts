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
import {
  DEVELOPMENT_BASELINE,
  earlierTags,
  overlaps,
  protocolRange as range,
} from "./compatibility-baseline";

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
const currentVersion = /^version = "([^"]+)"/m.exec(
  await readFile(join(root, "Cargo.toml"), "utf8"),
)?.[1];
const current = range(
  await readFile(join(root, "src/protocol/version.rs"), "utf8"),
);
let baseline = process.argv[2];
let baselineKind = "explicit reference";
if (!baseline) {
  const tags = (await run(["git", "tag"])).trim().split("\n").filter(Boolean);
  if (!currentVersion) throw new Error("No current release version");
  for (const tag of earlierTags(tags, currentVersion)) {
    // Only releases preceding the explicit wire contract are excluded.
    // Missing objects, malformed contracts and failed Git commands are errors.
    if (!(await run(["git", "ls-tree", tag, "src/protocol/version.rs"])).trim())
      continue;
    const previous = range(
      await run(["git", "show", `${tag}:src/protocol/version.rs`]),
    );
    if (overlaps(current, previous)) {
      baseline = tag;
      baselineKind = "release tag";
      break;
    }
  }
  if (!baseline) {
    baseline = DEVELOPMENT_BASELINE;
    baselineKind = "frozen development snapshot, not a published release";
  }
}
if (!baseline) {
  throw new Error(
    "No compatibility baseline; skipping is not a successful test",
  );
} else {
  console.log(`Compatibility baseline: ${baseline} (${baselineKind})`);
  const reference = (
    await run(["git", "rev-parse", "--verify", `${baseline}^{commit}`])
  ).trim();
  const previous = range(
    await run(["git", "show", `${reference}:src/protocol/version.rs`]),
  );
  if (!overlaps(current, previous))
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
