# Development and releases

English · [简体中文](development.zh-CN.md)

This guide covers building, testing, and releasing xrun, along with its architecture, protocol, and compatibility rules. For usage, see the [user manual](usage.md).

## Repository structure

| Path | Contents |
| --- | --- |
| `src/` | Core Rust package: CLI, daemon, and Linux relay in one executable |
| `desktop/` | Desktop app: React frontend in `src/`, Tauri 2 crate in `src-tauri/`, and build, package, release, and quality scripts in `scripts/` |
| `cloudflare/` | Cloudflare relay: Worker + Durable Object in TypeScript, deployment scripts, and workerd tests |
| `scripts/` | Installers: `install.sh` and `install.ps1` |
| `tests/` | Rust integration tests, installer tests, and fixtures |
| `docs/` | This guide and the user manual, in English and Chinese |

The core uses Rust. The desktop uses Tauri 2, React, TypeScript, and Vite. Bun manages dependencies, scripts, and tests for the frontend and Cloudflare subprojects. Release packages do not include Bun, and users need no Rust, Node.js, or Bun at runtime.

## Building

Install rustup. [rust-toolchain.toml](../rust-toolchain.toml) pins the Rust version and rustfmt/clippy components, so local builds, CI, and packaging use the same toolchain.

```bash
cargo build --locked --release
```

The executable is `target/release/xrun` (`xrun.exe` on Windows). To embed one-command Cloudflare deployment, package the Worker before building the CLI. Use the Bun version specified by `packageManager` in `cloudflare/package.json`:

```bash
bun install --cwd cloudflare --frozen-lockfile
bun run --cwd cloudflare package
XRUN_CLOUDFLARE_BUNDLE_DIR="$PWD/cloudflare/.wrangler/package" cargo build --locked --release
```

The build script validates the Worker module, version, protocol range, and SHA-256 manifest before embedding it. The public OAuth Client ID, scopes, and callback URLs come from [cloudflare/oauth.json](../cloudflare/oauth.json) and are validated and compiled into the executable. Rebuild after changing them; installed executables do not read the project's configuration file.

## Desktop

Requires Rust and Bun, plus Xcode Command Line Tools on macOS or MSVC and WebView2 on Windows.

```bash
bun install --cwd desktop --frozen-lockfile
bun run --cwd desktop dev          # Development with frontend hot reload
bun run --cwd desktop check        # Formatting, type checks, and UI tests
bun run --cwd desktop build        # Package for the current platform
bun run --cwd desktop build --debug
bun run --cwd desktop build:ui     # Build the frontend only
```

| Platform | Output |
| --- | --- |
| macOS | `target/release/bundle/macos/xrun.app.zip`, with `xrun.app` also retained |
| Windows | Per-user installer EXE under `target/release/bundle/nsis/` |

- `desktop/scripts/desktop.ts` builds the matching daemon (helper) before invoking Tauri. App and helper versions must match. If `CARGO_TARGET_DIR` is set, outputs and caches use that directory.
- `--target` selects an architecture within the same OS. For example, on Apple Silicon, run `rustup target add x86_64-apple-darwin`, then `bun run --cwd desktop build --target x86_64-apple-darwin`. Output is under `target/<target>/release/bundle/`. Supported targets are `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`, and `aarch64-pc-windows-msvc`. Universal packages and cross-OS builds are unsupported. Windows cross-architecture builds require the matching MSVC tools and Windows SDK.
- Local macOS packages use ad-hoc signing by default; set `APPLE_SIGNING_IDENTITY` for a specific identity. Official packages are Developer ID signed, notarized, and stapled in the Package workflow before ZIP creation. Windows installers are currently unsigned.
- Development mode can start and stop the daemon from the UI, reuses the local identity and settings, and logs to `~/.xrun/daemon-dev.log`. The daemon keeps running after the UI closes; development mode does not enable login startup. Login startup requires a packaged app.
- `tauri.macos.conf.json` selects the app bundle; `tauri.windows.conf.json` selects NSIS. The bundle identifier is `sh.xrun`; the macOS service label is `sh.xrun.daemon`.
- `lightweight.rs` owns webview creation, idle deadlines, and destruction. The main window uses `create: false`; native tray actions create it on demand. A renderer acknowledgement protects operations spanning multiple IPC calls, and native checks reject stale acknowledgements after reopening or while actions/dialogs are active. Settings persist in `desktop-lightweight.json`; the native tray host and daemon remain independent of the webview. Native UI tests cover cold background startup, release, and repeated recreation.

**Terminal CLI.** On launch, the macOS app links `~/.local/bin/xrun` to its bundled helper and adds an idempotent PATH block to zsh configuration (`.zprofile` and `.zshrc`, respecting `ZDOTDIR`) and Bash configuration. `--install-cli` performs the same setup without opening the UI for installer use. Development builds, mounted disk locations, and AppTranslocation locations do not change terminal configuration. The Linux installer configures PATH similarly. In Windows packages, `cli/xrun.exe` is the console CLI and root-level `xrun.exe` is the helper without a console. The installer invokes the app's `--install-cli` to update `HKCU\Environment\Path` and records its added entry in `.xrun-cli-path.json`, so uninstall removes only that entry. Setup failures report `CLI_INSTALL_FAILED`; the Windows installer exits with code 34.

**macOS service troubleshooting.** `/Applications/xrun.app/Contents/MacOS/xrun-desktop --start-service` uses the UI's service startup flow and prints status without opening a window. `launchctl print gui/$(id -u)/sh.xrun.daemon` shows registration and process state. SMAppService's `enabled` status only indicates registration and approval, not that launchd holds the current helper signature. After replacing the app, unregister and register again as required by Apple. Local ad-hoc checks do not replace acceptance testing with an officially notarized package.

## Cloudflare subproject

The Worker in `cloudflare/` shares device authentication and end-to-end session protocols with the Rust relay.

```bash
bun install --cwd cloudflare --frozen-lockfile
bun run --cwd cloudflare check          # TypeScript checks
bun run --cwd cloudflare test           # Tests in workerd
bun run --cwd cloudflare test:interop   # Local workerd with real Rust CLI and daemon
```

`test:interop` builds the CLI and tests with Cargo, copies them to a temporary directory, and runs isolated checks for network creation, pairing, reliable jobs, a 65 MiB file round trip, binary streams, TCP half-close, manager downtime, idle recovery, and revocation. It needs no Cloudflare credentials.

**Deployment from source** is for developers and requires a signed-in Wrangler:

```bash
bun run --cwd cloudflare deploy --name xrun-relay
bun run --cwd cloudflare remove --name xrun-relay
```

Set `CLOUDFLARE_ACCOUNT_ID` when using multiple accounts. Private state lives in `cloudflare/.deploy/` (do not commit it), separately from the user CLI's deployment records. The removal script accepts only recorded instances. After removal, use a different name for a new deployment.

**OAuth client.** The CLI uses xrun's registered public OAuth client; registration parameters are in [cloudflare/oauth-client.json](../cloudflare/oauth-client.json): Authorization Code + PKCE S256, token endpoint auth method `none`, and no distributed client secret. Default scopes are `account-settings.read` and `workers-scripts.admin` (Workers Admin is required for initial Worker creation and removal). `relay login --remember` additionally requests `offline_access`. Automatic deployment uses workers.dev only and does not request route-write permission. The client registers these callbacks:

```text
http://127.0.0.1:8976/oauth/callback
http://127.0.0.1:8977/oauth/callback
http://127.0.0.1:8978/oauth/callback
```

Login tries these ports in order and returns `INVALID_PORT` if all are occupied. Requested scope IDs must match the registered client; `.edit`, `.write`, and `.admin` are not interchangeable. For `OAUTH_INVALID_SCOPE`, inspect `error_description`. Use `GET /client/v4/oauth/scopes` to match display names to IDs, and update both JSON files together. For development, override the Client ID with `XRUN_CLOUDFLARE_OAUTH_CLIENT_ID`. Forks should register their own client and update `oauth.json`.

**Deployment details.** Before uploading, check the account's workers.dev subdomain. Treat it as uninitialized and attempt registration only when the subdomain query returns 400/404 with `10007`, or 403 with `10063`, and every returned error has that code. Ordinary 401/403 responses still abort deployment. Upload the Worker with the two SQLite DO classes `XrunRelay` and `XrunData`; the first deployment declares both in one v1 migration. Permanent deletion uses a removed marker. Deployment writes `XRUN_PROTOCOL_MIN` and `XRUN_PROTOCOL_MAX`, and a random ownership marker identifies the local deployment remotely. `src/deployment.rs` produces shared progress events: the CLI emits text or NDJSON, and the desktop receives Tauri events with operation IDs.

**Live deployment integration tests.** Store the complete address in a file outside the repository with permissions 0600:

```bash
XRUN_TEST_CF_LINK_FILE=/absolute/path/to/private-link-file \
  cargo test --locked --test cloudflare -- --ignored --nocapture
```

The tests use the real CLI and daemon to create an isolated network and cover jobs, 65 MiB files, streaming, forwarding, manager downtime, idle recovery, and revocation. Local workerd does not replace real-environment hibernation checks: use `cloudflare/scripts/hibernation.ts` with a temporary deployment of `cloudflare/tests/workerd/deployed-probe.ts` to verify reconstruction after hibernation, then remove the temporary Worker.

## Tests and CI

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
# Linux X11 screenshots; requires Xvfb, libX11, and libXrandr
xvfb-run -a -s '-screen 0 1024x768x24' cargo test --locked --test screenshot -- --ignored
bun desktop/scripts/check-quality.ts
```

- Unit tests live in `#[cfg(test)] mod tests` at the end of source files; integration tests live in `tests/`. Native desktop UI tests must run on the main thread; their entry point is `desktop/src-tauri/tests/native_ui.rs` (`harness = false`).
- `tests/cli_docs.rs` and `tests/smoke.rs` verify that the embedded manuals match `docs/usage.md` and `docs/usage.zh-CN.md`, and every `xrun doc` chapter exists exactly once in each language. If you change level-two headings, update the chapter table in `src/cli/docs.rs` and these tests. English is the default regardless of system locale; `--lang zh` selects Chinese for complete manuals, chapters, and chapter lists.
- Changes to signed record structures or encoding must update `tests/fixtures/signatures.json`. Field order changes must not accidentally change signed bytes.

Functional output-visibility and authorization-invalidation tests use a 5-second bound on shared CI. Strict acceptance limits of 100 ms for output visibility and 200 ms for authorization invalidation must be checked separately on an idle machine:

```bash
XRUN_LATENCY_OUTPUT=target/latency.json \
  cargo test --locked --test latency -- --ignored --nocapture --test-threads=1
```

| Workflow | Trigger | Coverage |
| --- | --- | --- |
| [Test Linux](../.github/workflows/test-linux.yml) | push, PR | x86_64 and arm64: fmt, clippy, all tests with a 1 MiB main-thread stack, X11 screenshots, real systemd service, workerd tests and interop, installers |
| [Test macOS](../.github/workflows/test-macos.yml) | push, PR | ARM: core tests, desktop checks, debug package and helper smoke, installers and rollback |
| [Test Windows](../.github/workflows/test-windows.yml) | push, PR | x86_64 and arm64: core tests, desktop checks, NSIS installation, silent updates and uninstall |
| [Readability](../.github/workflows/quality.yml) | push, PR | Source line length and file size, Cloudflare discriminated unions and non-null assertions, plus tests for the checker itself |
| [Package](../.github/workflows/package.yml) | Manual | Builds six release artifact sets; see Releases below |
| [Release](../.github/workflows/release.yml) | Manual | Creates a Release from a successful Package run |

Use the Actions results for the specific commit to determine CI status. Failed workflows upload test subprocess logs.

## Releases

**1. Synchronize versions.** `release-assets.ts` checks that these match: `Cargo.toml`, `desktop/src-tauri/Cargo.toml`, the two local packages in `Cargo.lock`, `desktop/package.json`, and `desktop/src-tauri/tauri.conf.json`.

**2. Package.** Manually run Package on main:

```bash
gh workflow run package.yml --ref main
```

Package builds six artifact sets: x86_64 and arm64 for Linux, macOS, and Windows. It embeds the Cloudflare Worker and runs `relay deploy --provider cloudflare --dry-run --json` using the installed helper or CLI to verify version, digest, and embedded OAuth configuration, with the runtime Client ID override removed. Linux and Windows build and check on native runners of the corresponding architecture. Both macOS builds use ARM runners; the Intel build runs smoke and installation checks through Rosetta, which does not replace testing on an Intel Mac. macOS signing and notarization require the credentials listed in the workflow. Package has read-only repository access and produces Actions artifacts only.

| Platform identifier | Download | Manifest |
| --- | --- | --- |
| `linux-x86_64` / `linux-arm64` | `xrun-linux-<arch>.tar.gz`, containing only `xrun` | `xrun-linux-<arch>.json` |
| `darwin-x86_64` / `darwin-arm64` | `xrun-app-darwin-<arch>.zip`, containing only `xrun.app` | `xrun-darwin-<arch>.json` |
| `windows-x86_64` / `windows-arm64` | `xrun-app-windows-<arch>.exe` | `xrun-windows-<arch>.json` |

Artifact architectures use `x86_64` and `arm64` (Rust targets use `aarch64`). [package-manifest.ts](../desktop/scripts/package-manifest.ts) records versions, components, and SHA-256 hashes. To prepare an equivalent manifest locally, run `bun desktop/scripts/package-manifest.ts --platform darwin-x86_64 --directory dist`.

**3. Publish.** Manually run Release with the successful Package run ID:

```bash
gh workflow run release.yml --ref main -f package_run_id=<run-id>
```

Release verifies that the Package run came from this repository's main branch and fully succeeded, checks out that commit, and downloads all six artifact sets without rebuilding. [release-assets.ts](../desktop/scripts/release-assets.ts) rechecks manifests, SHA-256, installers, and source versions. It creates a `v<version>` Release pointing to that commit and publishes it after uploading all files. Versions with a prerelease identifier are marked prerelease. An existing tag causes an error rather than being overwritten; handle any draft left by a failed upload before retrying. A Release contains six downloads, six manifests, and shared `install.sh`, `install.ps1`, `LICENSE`, and `SHA256SUMS` files. Only the publishing job has repository write access.

**Embedded manuals.** `docs/usage.md` (English) and `docs/usage.zh-CN.md` (Chinese) are compiled into the CLI. `xrun doc` defaults to English; `xrun doc --lang zh` selects Chinese, including chapter and `--list` output. Rebuild after editing either file. The README and both guides have language links. Release notes reference `usage.md#installation`; keep external website references aligned with the appropriate language file and heading when updating them.

**Before release:**

- All three platform CI workflows pass for the corresponding commit.
- Test network creation, joining, invitations, revocation, Cloudflare browser authorization, and live deployment with officially signed and notarized apps.
- Run the live Cloudflare integration tests.
- Check service and connection recovery after Mac login, sleep/wake, and cloud-host reboot. No physical Windows test environment is currently available; Windows login services, named pipes, screenshots, and screen locking have CI automation coverage only.
- Compatibility changes follow the rules below.

## Architecture

### Components

```text
Developer / AI → Local CLI → Local daemon (session reuse)
                    │ outbound connection
                    ▼
     Relay (Rust or Cloudflare): authenticate members, connect peers, forward ciphertext
                    ▲
                    │ outbound connection
               Target daemon

Local CLI / daemon ←── Device-to-device mutual TLS inside the relay tunnel ──→ Target daemon
```

| Component | Responsibilities and data |
| --- | --- |
| Relay | Authenticates member certificates, connects peers, and forwards ciphertext; stores network ownership, root-signed roster and lease, and traffic statistics; stores no jobs, logs, or files |
| Manager | Holds the network root private key and authoritative member database (`manager/`); issues member certificates, rosters, leases, and invitations |
| Daemon | Verifies source identities through end-to-end certificates, checks local permissions, executes jobs, transfers files, captures screenshots, and forwards ports; stores Jobs, logs, and attachments; caches sessions for the local CLI through private IPC |
| CLI | Initiates calls, stores local submission records (`submissions.sqlite`), and retrieves files |
| Desktop app | Manages the local device through the core library's public boundary and reads local Jobs; a separate daemon still performs execution |

Modules follow responsibilities: `membership` handles signed rosters and authoritative state; `network` handles creation, joining, pairing, renewal, and endpoint synchronization; `relay` implements the Rust relay; `secure` establishes end-to-end TLS; `ipc`, `session`, `pool`, and `multiplex` handle local communication and connection reuse; `daemon` and `store` manage reliable jobs; `process` manages native processes; `transfer`, `screenshot`, `forwarding`, and `streaming` implement operations; `service`, `control`, and `history` provide local services and read-only activity; `cli` provides command entry points. Public boundaries are `xrun::client` (user operations, settings, and read-only history), `xrun::runtime` (process entry points), `protocol`, and `error`. `xrun::testing` is for integration tests only. `deny(unreachable_pub)` prevents accidental exposure, and new request kinds must participate in exhaustive dispatch.

### Connections

1. **Outer transport**: devices connect to the relay over HTTPS/WSS. Rust relays pin certificates using the CA fingerprint in the deployment link; Cloudflare uses public WebPKI. The relay issues a random challenge for each status, control, and connection request. Members sign with their private keys, binding the network, device ID, endpoint path, and challenge. The manager additionally signs with the root key, allowing the relay to identify it. Connections without member certificates can reach only the manager, for joining and expired-certificate renewal.
2. **Control connection**: each daemon maintains a control connection for session notifications. Only a connection holding the device private key may claim its route. Ping runs every 15 seconds; no response for 45 seconds marks it offline. Reconnection uses exponential backoff up to 30 seconds.
3. **End-to-end transport**: once connected through the relay, devices establish mutual TLS inside the tunnel and verify the network root, member certificates, and expected device ID. They exchange roster digests first, sending a complete root-signed roster only when different. Source identity comes from the client certificate; relay routing IDs are used only for routing.
4. **Reuse**: the daemon retains a channel per device pair and uses yamux for up to 32 operation streams (8 MiB receive window, 16 KiB frames). Before each application request, the stream rechecks permissions, pause generation, certificate expiry, roster lease, and database. Channels close after 5 minutes idle; at most 3 idle channels are retained. The CLI borrows channels through private IPC: Unix sockets in a private directory with user checks, or Windows named pipes with user SID checks. Without a local daemon, the CLI connects directly. The connection layer never automatically replays submitted requests.
5. **LAN direct connections** (optional): listen only on a selected private address. Candidate addresses are exchanged only through authenticated end-to-end connections. Security checks match the relay path; failure falls back to the relay before an application request.

### Identities, rosters, and revocation

- Network IDs are `net_<base32 SHA-256 fingerprint of the root public key>`. Device IDs are random 32-character hexadecimal strings; names are aliases.
- A root-signed roster contains the network ID, increasing version, manager device ID, authority generation, members (ID, name, public key fingerprint, revocation state), relay address, and relay CA. It contains no access permissions. Devices retain the highest observed version in `roster.db`; old versions cannot overwrite it, and different contents at the same version return `ROSTER_CONFLICT`. A network supports at most 256 members including revoked ones and 256 active invitations.
- Revocation commits and signs locally on the manager before publication to the relay. The relay and online devices acknowledge the version and hash; the manager reports devices that have not received it. A signature proves the roster came from the manager, not that it is the latest globally.
- Roster leases are root-signed and bind the roster version, authority generation, and hash. They default to 15 minutes and renew every third of the duration. Devices and relays reject new operations against expired rosters with `ROSTER_EXPIRED`, bounding how long a relay can withhold revocations.
- The network root is valid for approximately 20 years. Member certificates last at most 365 days; the daemon renews them through the manager when fewer than 30 days remain.
- Access codes have eight characters from `23456789ABCDEFGHJKLMNPQRSTUVWXYZ`. The first two identify a public session; the last six carry a 30-bit random secret used only for device-to-device SPAKE2 verification. Each code permits at most 5 attempts, persistently deducted in the manager database. The relay stores only the public identifier, invitation ID, and expiry. After verification, fingerprint-pinned TLS delivers a strong random invitation and issues the identity.
- Manager backups use Argon2id (32 MiB, 3 iterations) for key derivation and AES-256-GCM encryption, with a 32 MiB maximum archive size. Restoration and handover advance the authority generation and preserve later revocations. A stale root-key holder automatically deletes its root private key.

### Authorization

Each daemon stores permissions by device ID in `daemon.toml`. Checks run in this order: valid, non-revoked member → not paused → no individual denial → allowlist or all-member trust. Managers accept individual permissions only. Job queries, logs, and cancellation also verify that the job belongs to the source. File locks serialize configuration changes. The daemon watches the file with 60 ms coalescing and falls back to polling every 2 seconds if watching fails. Read or validation failures deny access. Pause generations invalidate old sessions even after a rapid pause/resume.

### Job model

`exec`, `stream_exec`, `push`, `pull`, `screenshot`, and each `forward` connection are Jobs on the target. [src/store/schema.sql](../src/store/schema.sql) is the sole schema definition.

- The acceptance transaction first deduplicates by `(source_device_id, request_id)`: matching IDs and request digests return the original Job; a different digest returns `REQUEST_CONFLICT`. It then checks concurrency limits and persists the Job before execution. A full target returns `DEVICE_BUSY` without writing a deduplication record.
- Each request carries the target's `db_id`. Rebuilding the job database makes old requests return `DB_RESET`. Before sending, the CLI records submission intent in `submissions.sqlite`, retained for 7 days.
- Exit code 75 means a request may have arrived but its result is unconfirmed. The CLI marks it as possibly sent before network writing starts; a write failure does not imply non-delivery.
- Late events cannot overwrite a persisted terminal state. After daemon crash recovery, unfinished Jobs become `lost`. The boot ID, PID, and process start time determine whether leftover processes can be cleaned up safely. Jobs do not rerun automatically.
- Output is persisted in batches at 32 KiB, 50 ms after the first byte, or EOF. Subscribers are notified only after the transaction succeeds. Transport uses binary data; CLI JSON and desktop APIs use Base64.
- File and log chunks record encoding, original length, and SHA-256. Automatic compression uses zstd only when it saves at least 1/8.
- Transfer resumption: the target stores checkpoints and staged content in `job-transfers/`; the client keeps immutable copies in `client-transfers/`. Every 1 MiB, content is fsynced before persisting the offset. Interrupted transfers become `interrupted` and continue only through explicit `resume`. They expire after 24 hours, with at most 32 staged transfers per target.
- Attachments are independent copies with metadata in `job_attachments`. Environment values, stdin, scripts, file contents, and image contents are not stored in the Job table.

### Relay implementations

**Rust relay** (`xrun relay`, Linux only): configuration is in `~/.xrun/config.toml`, transport CA and server certificates in `~/.xrun/server/`, network ownership in `owner.json`, and traffic in `traffic.sqlite` (WAL/NORMAL, batched every 10 seconds). Online connections and sessions exist only in memory. Limits are 512 TCP connections, 16 unauthenticated connections per IP, 16 sessions per source, 32 per target, and 4096 globally.

**Cloudflare relay**: each deployment has one fixed `XrunRelay` object for network ownership, control connections, pairing, global capacity, and traffic aggregation. Each permitted data channel uses an `XrunData` object named by a random session ID; the control object does not forward ciphertext. Source and target each receive a one-use ticket binding network, device, control generation, and expiry. A network allows 8 source sessions (up to 4 per authenticated source and 4 per anonymous IP, with the last slot reserved for the manager). Each direction permits 4 MiB of unacknowledged ciphertext; the receiver acknowledges every 256 KiB, and frames are limited to 64 KiB. WebSocket attachments retain negotiated versions, idle state, and traffic counters across hibernation. Traffic is aggregated into SQLite every 10 seconds and during queries or closure.

**Traffic statistics**: both implementations share `TrafficQuery` and `TrafficReport`, recording daily UTC inbound/outbound totals by network and device in `traffic_usage_daily`. Only valid ciphertext binary frames in bound sessions count; authentication, ACKs, cache announcements, and heartbeats do not. Recording failures keep forwarding and mark `complete: false`; unreadable storage returns `STORAGE_ERROR`.

### Versions and compatibility

| Version | Used for | Rule |
| --- | --- | --- |
| Release version | `xrun --version`, diagnostics, local IPC | CLI, app helper, and daemon on one device must match |
| Protocol version | Outer WSS (`X-Xrun-Protocol: min-max`), end-to-end roster exchange, pairing, and Ready | Negotiate the highest common version; no overlap returns `VERSION_MISMATCH` without application requests or invitation consumption |
| Signature format | Signed payload prefix `xrun/sig-v1/<purpose>\0` plus fixed JSON | Independent of release version; pinned by `tests/fixtures/signatures.json` |
| Database schema | Jobs, submissions, membership, and traffic databases | Accept only the current schema; unknown or unversioned databases return `DB_SCHEMA_MISMATCH` without automatic changes |

Before the stable release, protocol 1, signature format 1, and schema 1 represent the current baseline. Internal refactors update that baseline directly, without old-RC migrations or mixed-version support. Update all devices and relays together. Shared version numbers and protocol-range negotiation do not guarantee compatible application formats between RCs. Stable compatibility and upgrade policies will be defined at the stable release.

Ordinary messages ignore unknown fields that can safely be ignored but reject missing required fields and unknown message types. Signed records, certificate proofs, and local security configuration strictly reject unknown fields.

### Main limits

| Item | Limit |
| --- | --- |
| Single application message | 1 MiB |
| Reliable execution stdin and script | 1 MiB each |
| File transfers, streaming input, TCP forwarding | No fixed total size; 64 KiB chunks |
| Screenshots | 64 MiB encoded, at most 32 million pixels |
| Logs per job / total | 64 MiB / 1 GiB; finished-job logs retained for 7 days |
| Concurrent commands | Default 4, configurable 1–64 |
| Concurrent files and screenshots | 8 per device |
| Device-pair channels | 32 streams each, at most 3 idle channels |
| Members / active invitations | 256 each |
| Port-forward connections | 32 per forwarding process, 32 per daemon |
