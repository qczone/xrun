import { afterEach, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const macTest = test.skipIf(process.platform !== "darwin");
const directories: string[] = [];
const installer = resolve(import.meta.dir, "../scripts/install.sh");
const version = "1.2.3";

type Manifest = {
  version: string;
  platform: string;
  artifacts: { cli: { file: string; sha256: string } };
};

afterEach(() => {
  for (const directory of directories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

function fixture() {
  const directory = mkdtempSync(join(tmpdir(), "xrun-installer-platform-"));
  directories.push(directory);
  const mocks = join(directory, "bin");
  const artifacts = join(directory, "artifacts");
  const payload = join(directory, "payload");
  for (const path of [mocks, artifacts, payload]) mkdirSync(path);
  writeFileSync(
    join(mocks, "uname"),
    '#!/bin/bash\nif [[ "$1" == -s ]]; then printf "%s\\n" "$TEST_OS"; else printf "%s\\n" "$TEST_ARCH"; fi\n',
  );
  writeFileSync(
    join(mocks, "sysctl"),
    '#!/bin/bash\nprintf "%s\\n" "$TEST_ARM_CAPABLE"\n',
  );
  chmodSync(join(mocks, "uname"), 0o755);
  chmodSync(join(mocks, "sysctl"), 0o755);
  for (const arch of ["x86_64", "arm64"]) {
    // Shell payloads isolate architecture selection from native binary execution.
    writeFileSync(
      join(payload, "xrun"),
      `#!/bin/bash\n# ${arch}\nprintf 'xrun ${version}\\n'\n`,
    );
    chmodSync(join(payload, "xrun"), 0o755);
    const file = `xrun-darwin-${arch}.tar.gz`;
    const packed = spawnSync("tar", [
      "-C",
      payload,
      "-czf",
      join(artifacts, file),
      "xrun",
    ]);
    expect(packed.status).toBe(0);
    const sha256 = createHash("sha256")
      .update(readFileSync(join(artifacts, file)))
      .digest("hex");
    writeFileSync(
      join(artifacts, `xrun-darwin-${arch}.json`),
      JSON.stringify({
        schema: 1,
        version,
        platform: `darwin-${arch}`,
        artifacts: { cli: { file, sha256 } },
      }),
    );
  }
  return {
    directory,
    artifacts,
    run(arch = "arm64", armCapable = "1", args: string[] = [], os = "Darwin") {
      const result = spawnSync(
        "bash",
        [
          installer,
          "--version",
          version,
          "--component",
          "cli",
          "--install-dir",
          join(directory, "installed"),
          "--source-dir",
          artifacts,
          ...args,
        ],
        {
          encoding: "utf8",
          env: {
            ...process.env,
            PATH: `${mocks}:${process.env.PATH}`,
            HOME: directory,
            TEST_OS: os,
            TEST_ARCH: arch,
            TEST_ARM_CAPABLE: armCapable,
          },
        },
      );
      expect(result.error).toBeUndefined();
      return { code: result.status, data: JSON.parse(result.stdout) };
    },
    mutate(arch: string, change: (manifest: Manifest) => void) {
      const path = join(artifacts, `xrun-darwin-${arch}.json`);
      const manifest = JSON.parse(readFileSync(path, "utf8"));
      change(manifest);
      writeFileSync(path, JSON.stringify(manifest));
    },
  };
}

for (const [host, armCapable, override, expected] of [
  ["x86_64", "0", "", "x86_64"],
  ["arm64", "1", "", "arm64"],
  ["x86_64", "1", "", "arm64"],
  ["arm64", "1", "x86_64", "x86_64"],
]) {
  macTest(
    `installer selects ${expected} on ${host}, ARM capability ${armCapable}, override ${override || "auto"}`,
    () => {
      const f = fixture();
      const args = override ? ["--arch", override] : [];
      const result = f.run(host, armCapable, args);
      expect(result.code).toBe(0);
      expect(result.data.platform).toBe(`darwin-${expected}`);
      expect(readFileSync(result.data.executable, "utf8")).toContain(
        `# ${expected}`,
      );
      expect(f.run(host, armCapable, args).data.changed).toBe(false);
    },
  );
}

macTest(
  "installer rejects unsupported or incompatible architecture before installation",
  () => {
    const f = fixture();
    for (const [host, capability, args, os, code] of [
      ["x86_64", "0", ["--arch", "arm64"], "Darwin", "UNSUPPORTED_PLATFORM"],
      ["arm64", "1", ["--arch", "amd64"], "Darwin", "INVALID_ARGUMENT"],
      ["ppc", "0", [], "Darwin", "UNSUPPORTED_PLATFORM"],
      ["x86_64", "0", [], "Linux", "UNSUPPORTED_PLATFORM"],
    ] as const) {
      const result = f.run(host, capability, [...args], os);
      expect(result.code).toBe(1);
      expect(result.data.error.code).toBe(code);
    }
  },
);

for (const [name, change] of [
  [
    "version",
    (m: Manifest) => {
      m.version = "9.9.9";
    },
  ],
  [
    "platform",
    (m: Manifest) => {
      m.platform = "darwin-x86_64";
    },
  ],
  [
    "artifact architecture",
    (m: Manifest) => {
      m.artifacts.cli.file = "xrun-darwin-x86_64.tar.gz";
    },
  ],
  [
    "artifact path",
    (m: Manifest) => {
      m.artifacts.cli.file = "../xrun.tar.gz";
    },
  ],
  [
    "digest",
    (m: Manifest) => {
      m.artifacts.cli.sha256 = "invalid";
    },
  ],
] as const) {
  macTest(`installer rejects a mismatched manifest ${name}`, () => {
    const f = fixture();
    f.mutate("arm64", change);
    const result = f.run();
    expect(result.code).toBe(1);
    expect(result.data.error.code).toBe("INVALID_MANIFEST");
  });
}

macTest(
  "installer rejects corrupt selected-architecture bytes without changing the installed CLI",
  () => {
    const f = fixture();
    const installed = f.run();
    expect(installed.code).toBe(0);
    const before = readFileSync(installed.data.executable);
    writeFileSync(join(f.artifacts, "xrun-darwin-arm64.tar.gz"), "corrupt");
    const result = f.run();
    expect(result.code).toBe(1);
    expect(result.data.error.code).toBe("CHECKSUM_MISMATCH");
    expect(readFileSync(installed.data.executable)).toEqual(before);
  },
);
