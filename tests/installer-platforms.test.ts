import { afterEach, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  lstatSync,
  readFileSync,
  rmSync,
  symlinkSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const macTest = test.skipIf(process.platform !== "darwin");
const unixTest = test.skipIf(process.platform === "win32");
const directories: string[] = [];
const installer = resolve(import.meta.dir, "../scripts/install.sh");
const version = "1.2.3";

type Manifest = {
  version: string;
  platform: string;
  artifacts: Record<string, { file: string; sha256: string }>;
};

afterEach(() => {
  for (const directory of directories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

function fixture(os = "Darwin") {
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
  const system = os === "Darwin" ? "darwin" : "linux";
  const component = os === "Darwin" ? "app" : "cli";
  function mock(name: string, source: string) {
    writeFileSync(join(mocks, name), `#!/bin/bash\n${source}\n`);
    chmodSync(join(mocks, name), 0o755);
  }
  mock("codesign", "exit 0");
  if (process.platform === "darwin") {
    mock("sha256sum", 'exec shasum -a 256 "$@"');
    mock("stat", 'for path; do :; done; exec /usr/bin/stat -f %Lp "$path"');
    mock(
      "readlink",
      "for path; do :; done; exec python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' \"$path\"",
    );
  }
  function pack(arch: string, marker = "") {
    // Shell payloads isolate architecture selection from native binary execution.
    const container = join(payload, arch);
    const binaryDirectory =
      os === "Darwin" ? join(container, "xrun.app/Contents/MacOS") : container;
    mkdirSync(binaryDirectory, { recursive: true });
    writeFileSync(
      join(binaryDirectory, "xrun"),
      `#!/bin/bash\n# ${arch} ${marker}\nif [[ "$0" == "\${TEST_BAD_EXECUTABLE:-}" && "$1" == --version ]]; then printf 'xrun wrong-version\\n'; else printf 'xrun ${version}\\n'; fi\n`,
    );
    chmodSync(join(binaryDirectory, "xrun"), 0o755);
    const file =
      os === "Darwin"
        ? `xrun-app-darwin-${arch}.zip`
        : `xrun-linux-${arch}.tar.gz`;
    if (os === "Darwin") {
      writeFileSync(
        join(binaryDirectory, "xrun-desktop"),
        `#!/bin/bash\nif [[ "$1" == --install-cli ]]; then exit 0; fi\nhelper="\$(dirname "$0")/xrun"\n[[ \$("$helper" --version) == 'xrun ${version}' ]] || exit 1\nprintf '{"version":"${version}"}\\n'\n`,
      );
      chmodSync(join(binaryDirectory, "xrun-desktop"), 0o755);
    }
    rmSync(join(artifacts, file), { force: true });
    const packed =
      os === "Darwin"
        ? spawnSync("ditto", [
            "-c",
            "-k",
            "--keepParent",
            join(container, "xrun.app"),
            join(artifacts, file),
          ])
        : spawnSync("tar", [
            "-C",
            container,
            "-czf",
            join(artifacts, file),
            "xrun",
          ]);
    expect(packed.status).toBe(0);
    const sha256 = createHash("sha256")
      .update(readFileSync(join(artifacts, file)))
      .digest("hex");
    writeFileSync(
      join(artifacts, `xrun-${system}-${arch}.json`),
      JSON.stringify({
        schema: 1,
        version,
        platform: `${system}-${arch}`,
        artifacts: { [component]: { file, sha256 } },
      }),
    );
  }
  for (const arch of ["x86_64", "arm64"]) pack(arch);
  return {
    directory,
    artifacts,
    mocks,
    pack,
    run(
      arch = "arm64",
      armCapable = "1",
      args: string[] = [],
      operatingSystem = os,
      environment: Record<string, string> = {},
    ) {
      const result = spawnSync(
        "bash",
        [
          installer,
          "--version",
          version,
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
            ZDOTDIR: directory,
            TEST_OS: operatingSystem,
            TEST_ARCH: arch,
            TEST_ARM_CAPABLE: armCapable,
            ...environment,
          },
        },
      );
      expect(result.error).toBeUndefined();
      return { code: result.status, data: JSON.parse(result.stdout) };
    },
    mutate(arch: string, change: (manifest: Manifest) => void) {
      const path = join(artifacts, `xrun-${system}-${arch}.json`);
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
      ["x86_64", "0", [], "FreeBSD", "UNSUPPORTED_PLATFORM"],
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
      m.artifacts.app.file = "xrun-app-darwin-x86_64.zip";
    },
  ],
  [
    "artifact path",
    (m: Manifest) => {
      m.artifacts.app.file = "wrong-file.zip";
    },
  ],
  [
    "digest",
    (m: Manifest) => {
      m.artifacts.app.sha256 = "invalid";
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
  "installer rejects corrupt selected-architecture bytes without changing the installed App",
  () => {
    const f = fixture();
    const installed = f.run();
    expect(installed.code).toBe(0);
    const before = readFileSync(installed.data.executable);
    writeFileSync(join(f.artifacts, "xrun-app-darwin-arm64.zip"), "corrupt");
    const result = f.run();
    expect(result.code).toBe(1);
    expect(result.data.error.code).toBe("CHECKSUM_MISMATCH");
    expect(readFileSync(installed.data.executable)).toEqual(before);
  },
);

for (const [host, expected] of [
  ["x86_64", "x86_64"],
  ["aarch64", "arm64"],
]) {
  unixTest(`Linux installer selects the native ${expected} CLI`, () => {
    const f = fixture("Linux");
    const result = f.run(host);
    expect(result.code).toBe(0);
    expect(result.data.component).toBe("cli");
    expect(result.data.platform).toBe(`linux-${expected}`);
    expect(f.run(host).data.changed).toBe(false);
    const shell = spawnSync(
      "bash",
      ["--noprofile", "--norc", "-c", '. "$HOME/.profile"; xrun --version'],
      { encoding: "utf8", env: { ...process.env, HOME: f.directory } },
    );
    expect(shell.status).toBe(0);
    expect(shell.stdout.trim()).toBe(`xrun ${version}`);
  });
}

unixTest(
  "Linux installer preserves shell content, symlinks and permissions",
  () => {
    const f = fixture("Linux");
    const profile = join(f.directory, "profile-target");
    writeFileSync(profile, "# keep this profile\n", { mode: 0o640 });
    symlinkSync(profile, join(f.directory, ".profile"));
    const installDirectory = join(f.directory, "CLI with 'quotes' and spaces");
    const args = ["--install-dir", installDirectory];
    expect(f.run("x86_64", "0", args).code).toBe(0);
    const before = readFileSync(profile, "utf8");
    expect(before).toStartWith("# keep this profile\n");
    expect(f.run("x86_64", "0", args).code).toBe(0);
    expect(readFileSync(profile, "utf8")).toBe(before);
    expect(lstatSync(join(f.directory, ".profile")).isSymbolicLink()).toBe(
      true,
    );
    expect(statSync(profile).mode & 0o777).toBe(0o640);
    const shell = spawnSync(
      "bash",
      ["-c", '. "$HOME/.profile"; xrun --version'],
      {
        encoding: "utf8",
        env: { ...process.env, HOME: f.directory },
      },
    );
    expect(shell.status).toBe(0);
    expect(shell.stdout.trim()).toBe(`xrun ${version}`);
  },
);

macTest("Linux manifest parsing also works with Python 3 and no jq", () => {
  const f = fixture("Linux");
  const result = f.run("aarch64", "0", [], "Linux", {
    PATH: `${f.mocks}:/usr/bin:/bin:/usr/sbin:/sbin`,
  });
  expect(result.code).toBe(0);
  expect(result.data.platform).toBe("linux-arm64");
});

unixTest(
  "Linux installer rejects a mismatched manifest and a corrupt download",
  () => {
    const f = fixture("Linux");
    const installed = f.run();
    expect(installed.code).toBe(0);
    const before = readFileSync(installed.data.executable);
    f.mutate("arm64", (manifest) => {
      manifest.platform = "linux-x86_64";
    });
    expect(f.run().data.error.code).toBe("INVALID_MANIFEST");
    f.pack("arm64");
    writeFileSync(join(f.artifacts, "xrun-linux-arm64.tar.gz"), "corrupt");
    expect(f.run().data.error.code).toBe("CHECKSUM_MISMATCH");
    expect(readFileSync(installed.data.executable)).toEqual(before);
  },
);

unixTest(
  "Linux PATH configuration failure removes an incomplete new installation",
  () => {
    const f = fixture("Linux");
    const profile = join(f.directory, ".bashrc");
    writeFileSync(profile, "# read-only profile\n", { mode: 0o444 });
    const result = f.run();
    expect(result.code).toBe(1);
    expect(result.data.error.code).toBe("CLI_INSTALL_FAILED");
    expect(readFileSync(profile, "utf8")).toBe("# read-only profile\n");
    expect(existsSync(join(f.directory, "installed/xrun"))).toBe(false);
  },
);

for (const os of ["Darwin", "Linux"]) {
  const platformTest = os === "Darwin" ? macTest : unixTest;
  platformTest(
    `${os} installer restores the old program after a failed update`,
    () => {
      const f = fixture(os);
      const installed = f.run();
      expect(installed.code).toBe(0);
      const before = readFileSync(installed.data.executable);
      f.pack("arm64", "candidate update");
      const result = f.run("arm64", "1", [], os, {
        TEST_BAD_EXECUTABLE: installed.data.executable,
      });
      expect(result.code).toBe(1);
      expect(result.data.error.code).toBe("SELF_CHECK_FAILED");
      expect(readFileSync(installed.data.executable)).toEqual(before);
    },
  );
}
