# xrun user manual

English · [简体中文](usage.zh-CN.md)

xrun runs programs, manages background jobs, transfers files, captures screenshots, and forwards ports between your own macOS, Windows, and Linux devices. This manual covers installation, networking, permissions, and everyday use.

Use `xrun help [command]` for command reference, `xrun doc` for the complete offline manual, `xrun doc <topic>` for one chapter, and `xrun doc --list` to list chapters. English is the default; add `--lang zh` for Chinese, for example `xrun doc automation --lang zh`. Both manuals are embedded in the executable. Help and documentation require no identity, network, or running service, so AI agents can read them directly. For an automation workflow, start with `xrun doc automation`.

| Topic | Contents |
| --- | --- |
| `install` | Installation |
| `quickstart` | Quick start |
| `relay` | Relay deployment |
| `access` | Access control |
| `network` | Network management |
| `execute` | Running programs |
| `jobs` | Jobs and logs |
| `automation` | Automation and AI |
| `files` | Files and screenshots |
| `streaming` | Streaming execution |
| `forward` | Port forwarding |
| `desktop` | Desktop app |
| `config` | Configuration and services |
| `errors` | Status and troubleshooting |
| `upgrade` | Upgrading and removal |

## Installation

| Platform | Download | Contents |
| --- | --- | --- |
| macOS 13+ | `xrun-app-darwin-<arch>.zip` | Desktop app with CLI |
| Windows | `xrun-app-windows-<arch>.exe` | Per-user installer with desktop app and CLI |
| Linux | `xrun-linux-<arch>.tar.gz` | A single `xrun` executable |

Architectures are `x86_64` (AMD64; choose this for Intel Macs) and `arm64` (Apple Silicon, Windows ARM, and ARM Linux). Download the appropriate file from [Releases](https://github.com/xrun-sh/xrun/releases). The CLI, background service (daemon), and Linux relay use the same executable. Rust, Node.js, and Bun are not required at runtime.

- **macOS**: extract `xrun.app`, move it to `/Applications` or `~/Applications`, then open it. The first launch links the CLI to `~/.local/bin/xrun` and configures PATH for zsh and Bash. Open a new terminal to run `xrun --version`. Opening the app directly from Downloads may make macOS run it from a temporary translocation location, where the CLI entry is not created.
- **Windows**: run the installer. It installs the app and CLI and adds the installation's `cli` directory to your user PATH. Uninstalling removes only the PATH entry the installer added.
- **Linux**: place the archived `xrun` executable in a permanent location, such as `~/.local/bin/`, and add it to PATH. The service registers the executable's absolute path, so avoid moving it after installation.

### Command-line installation

Each Release includes `install.sh` (macOS and Linux) and `install.ps1` (Windows). The scripts detect the architecture, verify the version and SHA-256, and check the installed executable. On macOS, they also check the signature. They are suitable for AI agents and automation. Replace `<version>` with the full Release version:

```bash
curl -fLO https://github.com/xrun-sh/xrun/releases/download/v<version>/install.sh
bash install.sh --version <version>
```

```powershell
Invoke-WebRequest https://github.com/xrun-sh/xrun/releases/download/v<version>/install.ps1 -OutFile install.ps1
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File .\install.ps1 -Version <version>
```

| install.sh | install.ps1 | Purpose |
| --- | --- | --- |
| `--version` | `-Version` | Required; the complete release version |
| `--install-dir` | `-InstallDir` | Parent installation directory |
| `--source-dir` | `-SourceDir` | Install offline from a local artifact directory |
| `--base-url` | `-BaseUrl` | Download from another HTTPS artifact directory |
| `--arch x86_64` | — | Install the Intel build on an Apple Silicon Mac with Rosetta |

Default locations: macOS uses `~/Applications/xrun.app` with a CLI entry at `~/.local/bin/xrun`; Windows uses `%LOCALAPPDATA%\Programs\xrun` with the CLI in its `cli` subdirectory; Linux uses `~/.local/bin/xrun`. Linux requires curl, tar, sha256sum, and either jq or Python 3.

The script writes one JSON object to stdout. On success, it includes the version, installation path, CLI `executable` path, `changed`, and self-check results. Windows also includes `helper_executable`. Reinstalling the same artifacts returns `changed: false`. On failure, `ok` is `false`, `error.code` and `error.message` describe the error, and the script exits with code 1.

For updates, close the macOS app first. The script stops the old service before replacing files and restores the old files if replacement or verification fails. Linux also stops the service first. Windows runs the installer silently: if service preparation fails, it leaves the executables in place and returns 32 for an update or 33 for an uninstall. Terminal configuration failure returns 34. Diagnostics are written to `xrun-install-error.log` in the installation directory. The scripts only install xrun; use the app or CLI to join a network and grant access.

## Quick start

An xrun network has a **relay** reachable by every device, a **manager device** that issues member identities, and other **member devices**. Devices make outbound connections to the relay, so they do not need inbound ports open.

Examples use the device names `macbook`, `ubuntu`, and `windows`; choose your own names if you prefer. Here, the Mac `macbook` is the manager and the Linux device `ubuntu` is a member. Install xrun on both first. Each step specifies where to run it.

**1. Prepare a relay.** Choose one deployment method; see `xrun doc relay` for details.

For Cloudflare, run this on the Mac and authorize xrun in your browser:

```bash
xrun relay deploy --provider cloudflare --name xrun-relay
```

For a self-hosted relay, run this on the Linux server:

```bash
xrun relay deploy --provider local --addr <server-ip>:9528
```

On success, the command returns a complete deployment link: an `https://…` address for Cloudflare or an `xrun-relay://…` link for a self-hosted relay. Save it for the next step on the manager device.

**2. Create the network on the Mac.** Replace `<deployment link>` with the complete link from the previous step:

```bash
xrun connect '<deployment link>' --name macbook
```

On success, xrun displays the local name and device ID. The Mac becomes the manager and starts its background service. Hosting the relay on a particular machine does not make that machine the manager. A relay permanently binds to the first network created on it and serves only that network.

**3. Generate an access code on the Mac:**

```bash
xrun invite
```

The command prints the relay address and an eight-character access code, such as `K7M4-P9Q2`. On Linux, replace the sample address and code with that output:

```bash
xrun connect relay.example.com K7M4-P9Q2 --name ubuntu --allow-manager
```

`--allow-manager` allows the manager to access this new device and grants no other permissions. If omitted, an interactive terminal asks for consent; `--json` and noninteractive calls treat omission as refusal. You can also use the short link `xrun://relay.example.com#K7M4-P9Q2` for the same code. For automation, generate a complete invitation with `xrun invite --link`, then run `xrun connect '<invitation>'`.

On success, xrun displays `ubuntu` and its device ID and starts the Linux background service.

**4. Back on the Mac, verify the setup:**

```bash
xrun status
xrun ubuntu -- uname -s
```

`status` should list `ubuntu` as `relay-connected`. If the remote command prints `Linux` and exits with code 0, the connection, permission, and execution checks have all succeeded.

For `SOURCE_NOT_ALLOWED`, run `xrun allow-from macbook` on Linux and retry. For `DEVICE_OFFLINE`, check the service on Linux with `xrun status`; if stopped, run `xrun daemon start`. See `xrun doc errors` for other errors.

A few rules:

- Access codes are valid for 10 minutes, can be used once, and allow at most 5 attempts. Case, spaces, and hyphens do not matter. Generating a new code invalidates the previous one. Restarting the manager's service also invalidates unused codes.
- The manager's service must be online when a device joins. Membership does not grant access; see `xrun doc access`.
- **Keep the manager online.** It regularly renews the roster lease, which defaults to 15 minutes. Once the lease expires while the manager is offline, new remote operations return `ROSTER_EXPIRED`. Accepted jobs keep running. See `xrun doc network`.
- For Linux containers without a systemd user service, add `--no-daemon` to prepare the identity only, then run `xrun daemon` under an external process manager.
- Explicit commands are also available: `xrun up --relay <deployment-link>` creates a network and immediately prints an invitation; `xrun join <invitation>` accepts member invitations only.
- The desktop app's **New network** flow combines steps 1 and 2. See `xrun doc desktop`.

## Relay deployment

A relay connects devices and forwards end-to-end encrypted data. Both relay implementations offer the same features:

| | Cloudflare | Self-hosted Linux |
| --- | --- | --- |
| Runs in | Your Cloudflare account (Worker + Durable Object) | A Linux host reachable by all devices |
| Deployment command | Runs on macOS, Windows, or Linux | Runs on the relay host; requires systemd |
| Address | `https://<name>.<subdomain>.workers.dev` or a custom domain | `xrun-relay://<address>/<certificate-fingerprint>` |
| Concurrent sessions | 8 per network | 16 per source, 32 per target |

### Cloudflare

```bash
xrun relay deploy --provider cloudflare --name xrun-relay
```

The command opens a browser for Cloudflare sign-in and authorization. If you have multiple accounts, choose one in the terminal or pass `--account <account-id>`. The CLI uploads its embedded Worker directly; source code, Node.js, and Wrangler are not required. If the account has no workers.dev subdomain, xrun creates one. If Cloudflare still requires initialization, open Workers & Pages in the dashboard once and retry.

- On success, the command prints the complete HTTPS address. Deploying again with the same name updates the code while retaining the address and data. If a Worker with that name belongs to another application, it returns `ALREADY_EXISTS` without overwriting it.
- `--region` provides a location hint for new storage objects: `auto` (default, based on the connecting location), `wnam`, `enam`, `sam`, `weur`, `eeur`, `apac`, `apac-ne`, `apac-se`, `oc`, `afr`, or `me`. It only affects objects when first created; Cloudflare decides their actual location.
- Authorization is temporary by default and is revoked when the operation finishes. Deployment records are stored in `~/.xrun/cloudflare/`; keep them, as updates and removal depend on them. For regular maintenance, after creating the network, run `xrun relay login --provider cloudflare --remember` on the manager to save the login in the system credential store (macOS Keychain, Windows Credential Manager, or Linux Secret Service). Other devices cannot remember a login. `xrun relay logout --provider cloudflare` revokes and removes the login while keeping the deployed relay.
- Custom domains: in the Cloudflare dashboard, go to Worker → Settings → Domains & Routes and add a Custom Domain from a zone in your account. Use `https://relay.example.com` when creating the network. To change an existing network's address, stop the service on the manager, run `xrun up --relay 'https://relay.example.com'`, then restart the service.
- Each direction allows up to 4 MiB of unacknowledged data in flight. Individual files have no fixed size limit.

### Self-hosted Linux relay

Run this on the relay host:

```bash
xrun relay deploy --provider local --addr 203.0.113.10:9528
```

The command installs a systemd user service and enables linger, so the relay starts at boot and restarts after unexpected exits. It listens on TCP 9528 by default and supports IPv4 only. Open the port in your firewall and security group yourself.

| Option | Purpose |
| --- | --- |
| `--addr <host>:<port>` | Published device connection address; repeat or comma-separate values. Explicit addresses are persisted. Domains need an A record. |
| `--port <port>` | Listening port, default 9528; may differ from the published port when using port mapping |
| `--no-detect` | When `--addr` is omitted, use interface addresses only without querying the public egress IP |

Without `--addr`, xrun detects interface IPv4 addresses and the public egress address. An egress address is not necessarily reachable from outside; use a fixed IP or domain for long-term deployments. The deployment link contains the relay certificate fingerprint, remains unchanged after restarts, and is reusable. It is not an access credential. The relay host can separately join as a regular member; the roles are independent.

### Inspecting and removing a relay

```bash
xrun relay link --provider cloudflare --name xrun-relay    # Read the local record; no network
xrun relay check --provider cloudflare --name xrun-relay   # Check connectivity and protocol; up to 30 s
xrun relay remove --provider cloudflare --name xrun-relay --purge
xrun relay link --provider local
xrun relay remove --provider local
```

`relay check` does not reauthorize, upload code, or create a network. Removing a Cloudflare relay requires `--purge` and permanently deletes the Worker, all relay data, and traffic statistics. Removal only accepts an instance recorded locally whose remote ownership marker matches. Failed removal can be retried; once removal starts, use a new name for any new deployment. `remove --provider local` uninstalls the service but keeps configuration and statistics. `xrun relay run --provider local` runs the configured local relay in the foreground for debugging or an external process manager.

### Deployment progress and automation

Normal mode writes stages and timing to stderr; stdout contains only the final address and next command. Scripts and AI agents should use `--json`:

```bash
xrun relay deploy --provider cloudflare --name xrun-relay --no-browser --json
xrun relay check --provider cloudflare --name xrun-relay --json
```

- stdout contains one final JSON object; stderr contains newline-delimited JSON events. Progress events use `event: deployment_progress` and include `operation_id`, `sequence`, `timestamp_ms`, `stage`, `status`, `elapsed_ms`, and `stage_elapsed_ms`. They include `authorization_url` while waiting for authorization, `accounts` when account selection is needed, and `deployment` and `next_command` after saving the relay. Events contain no authorization codes or tokens. Use stages and error codes rather than parsing message text.
- `--no-browser` only disables automatic browser opening. The command still waits for authorization for up to 5 minutes. Open the authorization page on the same machine running the command; the callback uses port 8976, 8977, or 8978 on `127.0.0.1`.
- For unattended deployment, provide an API token through `CLOUDFLARE_API_TOKEN` and an account through `--account` or `CLOUDFLARE_ACCOUNT_ID`. The token is not written to deployment records. In JSON mode, multiple accounts without an explicit selection return `INTERACTIVE_REQUIRED`.
- Results include `deployed`, `ready`, `authorization_cleanup`, and `next_command`. If the Worker was saved but connectivity checks or authorization cleanup fail, stdout still returns the created relay and an `error`, and the command exits nonzero. Keep the same deployment name: run `relay check` first, then `xrun up --relay <address> --name <device-name> --json` once it is ready.
- Ctrl+C cancels the operation and exits with code 130. Saved deployment records remain; continue with the same `--name`. After a forced exit or failed authorization revocation, you can manually revoke xrun's authorization in your Cloudflare account.
- `--dry-run --json` only checks the embedded Worker without signing in or changing Cloudflare.
- Stage timings are recorded in `~/.xrun/deployment.log`, capped at approximately 1 MiB, without authorization URLs, tokens, or error bodies.

## Access control

xrun separates three responsibilities: the manager decides **who is a member** through invitations and revocations; each target decides **who may access it**; the relay forwards connections. Membership alone grants no execution, file, screenshot, or forwarding permissions.

Grant access on the target device. For example, allow macbook to access this machine:

```bash
xrun allow-from macbook    # Allow access
xrun deny-from macbook     # Deny access and close existing sessions
xrun allow-from --all      # Trust all members, including future members
xrun deny-from --all       # Disable all-member trust; retain individual allows and denials
xrun daemon pause         # Pause all remote access
xrun daemon resume        # Resume with existing permissions
```

- **Allowing access grants the target's current-user permissions**: running commands, reading and writing files, capturing screenshots, and forwarding ports. Only allow trusted devices.
- Checks run in this order: non-revoked member → local access not paused → no individual denial → individually allowed or all-member trust enabled. Permissions are local and survive restarts.
- Access is directional. For mutual access, run `allow-from` for the other device on each machine. `xrun invite --allow` (or `up --allow`) merely asks the new device to allow the manager, subject to consent on that device. An invitation never grants the new member access to the manager.
- The manager rejects remote operations by default. It accepts individual `allow-from` permissions but rejects `allow-from --all` with `MANAGER_PROTECTED`. An allowed device can access the network management keys on the manager, so grant access carefully.
- Names are aliases. Permissions and records use immutable device IDs; commands also accept 32-character hexadecimal device IDs.
- Denial and pausing close existing sessions, streaming executions, and port forwards. Accepted reliable jobs continue running.

Only the manager can revoke a member:

```bash
xrun revoke windows
```

The manager signs a new roster, distributes it to the relay and online devices, and lists devices that have not acknowledged it. Devices that receive the update immediately reject the revoked device. Accepted jobs are not canceled or rolled back. Offline devices receive the update when they reconnect. For immediate blocking, deny the source or pause access on the target. A revoked key cannot rejoin.

### Security model

- Devices establish mutual TLS inside the relay tunnel and verify identities against the network's own root certificate. Only the two endpoints can read commands, arguments, environment variables, input, output, files, and screenshots.
- The relay can see the network ID, member certificates presented on connection, source IPs, online state, connection times, and traffic sizes. It stores network ownership, the manager-signed roster (device IDs, names, key fingerprints, and revocations), its lease, and traffic statistics. It cannot impersonate members, grant access, or decrypt application data, but it can interrupt connections or delay updates.
- Only the manager holds the network root private key. Losing it prevents further member issuance or revocation. Create an encrypted backup with `xrun network backup`.
- The local data directory `~/.xrun` contains private keys, databases, and logs; treat it as sensitive. Remote output is returned verbatim. Callers must handle terminal control characters and prompt injection themselves.

## Network management

`xrun network` manages roster leases, LAN direct connections, transfer compression, and the manager identity. The desktop app provides the same options in Settings.

### Roster lease

```bash
xrun network lease       # Read the current lease
xrun network lease 60    # On the manager, set 60 minutes (1–1440)
```

While online, the manager renews the roster approximately every third of its lease duration. The default lease is 15 minutes. Devices and relays only accept an unexpired roster. Once the manager has been offline past expiry, new remote operations, including job queries, return `ROSTER_EXPIRED`. Accepted jobs continue running, and local activity remains available. Renewal resumes automatically when the manager comes back online.

A longer lease lets the manager stay offline longer, but also lets a relay withhold revocation updates for longer. Keep device clocks synchronized. Member certificates are valid for at most 365 days and renew automatically when fewer than 30 days remain; renewal also requires the manager online.

### LAN direct connections

```bash
xrun network direct                 # List local private IPv4 interfaces and current settings
xrun network direct 192.168.1.10    # Enable direct connections on this private address
xrun network direct --off
```

When both devices select interfaces on the same LAN, connections try the direct path first. Direct mode listens on a random port only at the selected address. It does not scan the network or use UPnP. Addresses are exchanged only over authenticated, encrypted connections, with the same identity and permission checks as relay connections. If a direct connection fails, xrun falls back to the relay before sending the request. Direct traffic is excluded from relay statistics. Device information shows whether the last connection was direct or relayed.

### Transfer compression

```bash
xrun network compression          # Read the current setting
xrun network compression auto     # Default: compress only when saving at least 1/8
xrun network compression none     # No compression
xrun network compression zstd     # Always try zstd, without making data larger
```

The setting affects only files and logs sent by this device. Already compressed formats such as PNG, JPEG, WebP, ZIP, GZIP, Zstd, and MP4 are skipped. Compression does not change the bytes written to disk.

### Backing up and handing over management

The following commands prompt for a hidden password of at least 12 characters. Automation can use `--password-stdin` to read one line from stdin. Backups contain the manager identity, network root private key, and a member database snapshot. They do not include Cloudflare login credentials.

The examples use CLI service commands. For an app-managed service, use Settings → Network and service to stop and start it.

#### Backup and restore

Create an encrypted backup on the current manager, and store the file and password separately:

```bash
xrun network backup ./xrun-manager.backup
```

To restore, copy the backup to the recovery device, then run these commands there:

```bash
xrun daemon stop
xrun network restore ./xrun-manager.backup --json
xrun daemon start
xrun status --json
```

On a new installation with no registered service, omit `stop` and use `xrun daemon install` instead of `start` after restoration. On success, the returned `manager_id` from `restore` should match `local.device_id` in `status --json`. Once connected, `local.daemon_connected` should be `true`.

An incorrect password or damaged file leaves the existing identity intact. Restoration checks the latest roster on the relay, preserves subsequent revocations, and clears the access allowlist; grant access again with `allow-from`. If the same manager identity is restored onto another machine, the original machine deletes its root private key and stops its service when it observes the new authority generation.

#### Handover to an existing member

This example hands management to the Linux member `ubuntu`. First confirm that it allows access from the current manager. Then create and transfer the handover file on the **old manager**:

```bash
xrun network migrate ubuntu --out ./xrun-handover.backup
xrun ubuntu push ./xrun-handover.backup /tmp/xrun-handover.backup --no-overwrite
```

The old manager keeps working after the file is created. Once transfer succeeds, run these commands **locally on ubuntu** to stop the service, import the file, and restart:

```bash
xrun daemon stop
xrun network restore /tmp/xrun-handover.backup --json
xrun daemon start
xrun status --json
```

Confirm that `restore` returns a `manager_id` matching `local.device_id` in `status --json`, with `local.daemon_connected` set to `true`. Once the old manager receives the update, it revokes its Cloudflare login, deletes its root private key, and becomes a regular member. Other devices do not automatically grant access to ubuntu; run `allow-from ubuntu` on each target separately.

An incorrect password or failed validation preserves the existing identity, so you can correct the problem and import again. A network always has one manager. Multiple managers and root key rotation are not supported.

## Running programs

```bash
xrun ubuntu -C /home/user/demo -- cargo test
xrun windows -C 'D:\demo' -- cargo build
xrun ubuntu --env RUST_LOG=info --env MODE=test -C /home/user/demo -- ./demo
```

Arguments after `--` are passed unchanged to the remote program. xrun starts programs directly, without a shell; pipes, redirections, and variable expansion are handled by your local shell. The target searches its background service's PATH (see `xrun doc config`). Use an absolute executable path if it cannot find a program.

To use a shell, pass a script through stdin with `--script` and explicitly choose `sh`, `bash`, `zsh`, `powershell`, `pwsh`, or `cmd`:

```bash
xrun ubuntu -C /repo --script bash < ./build.sh
xrun windows -C 'D:\demo' --script powershell < ./build.ps1
printf 'hello\n' | xrun ubuntu --stdin -- cat
```

PowerShell scripts use UTF-8. cmd scripts accept ASCII only, and their arguments cannot contain double quotes or newlines. Run Windows batch files through `--script cmd` or `cmd.exe /C`; direct execution returns `SHELL_REQUIRED`.

| Option | Purpose |
| --- | --- |
| `-C <directory>` | Absolute remote working directory; defaults to the target's configured directory, initially its home directory |
| `--env KEY=VALUE` | Set a remote environment variable; repeatable |
| `--stdin` | Read all local stdin before submission and send it with the request, up to 1 MiB |
| `--script <shell>` | Read a script from stdin, up to 1 MiB |
| `--timeout <seconds>` | Execution timeout; foreground default 1800, 0 means unlimited |
| `--request-id <ID>` | Resume the same submission; see `xrun doc errors` |
| `-i` | Streaming execution; see `xrun doc streaming` |

Foreground execution forwards remote stdout and stderr unchanged and exits with the remote exit code. It is a **reliable job**: once accepted, the target runs it to completion and stores the result, even if the CLI disconnects. The CLI tries to reconnect and catch up on logs for 30 seconds; if it still cannot confirm the result, it exits with code 75.

By default, the target runs at most 4 commands concurrently, including streaming execution. It returns `DEVICE_BUSY` when full, without queuing. With the local background service running, the CLI reuses an encrypted connection to the same target to avoid repeated handshakes.

## Jobs and logs

`start` returns a job reference, such as `ubuntu/ABC123`, as soon as the job is accepted. Background jobs have no timeout by default:

```bash
xrun ubuntu start -C /home/user/demo -- ./server
xrun ubuntu jobs --running
xrun ubuntu jobs ABC123 --json
xrun ubuntu logs ABC123 --tail 40
xrun ubuntu logs ABC123 --follow
xrun ubuntu wait ABC123 --timeout 60
xrun ubuntu kill ABC123
```

Replace ABC123 with the returned six-character ID, or use the complete reference. Automation can get the job ID from `start --json`.

- `jobs` lists all operations this device submitted to the target: commands, streaming execution, file transfers, screenshots, and port forwarding. Pages default to 50 entries. Use `--limit` and `--offset` for pagination and `--running` or `--request-id` for filtering. An ID selects one job and cannot be combined with filters.
- `logs` applies only to regular commands. It reads a current snapshot by default. `--follow` catches up and keeps following; `--after N` starts after sequence N; `--tail N` asks the target for the last N lines. `--after` and `--tail` are mutually exclusive.
- `wait` waits for completion, then displays the last 40 lines (change with `--tail`). `--timeout` limits the wait without canceling the job. An unconfirmed result returns 75.
- `kill` requests cancellation and waits for confirmation. Unix sends SIGTERM, then SIGKILL after 5 seconds. Windows terminates the job's entire process tree.
- Each device can query and control only the jobs it submitted.
- `xrun recent` lists local submission records from the last 24 hours to recover a request_id. It does not report the current state of the remote job.

| State | Meaning |
| --- | --- |
| `accepted` / `running` | Accepted / running |
| `succeeded` | Succeeded |
| `failed` | Failed, including a nonzero exit; the result retains the exit code and error |
| `canceled` / `timed_out` | Canceled / timed out; all managed processes have stopped |
| `interrupted` | A file transfer was interrupted and awaits `resume` |
| `lost` | The result cannot be confirmed, for example after a target service crash; the operation may have run |

Jobs never rerun automatically. A normal target service shutdown cancels running jobs. After a crash and restart, unfinished jobs become `lost`. Logs are limited to 64 MiB per job (excess output is marked `TRUNCATED`) and 1 GiB total. Logs for finished jobs are retained for 7 days; job records themselves are retained indefinitely.

## Automation and AI

Use `xrun doc --list` to find topics and `xrun help <command>` to check arguments. After networking is configured, AI agents and scripts can follow this workflow. The example target is an authorized Linux device named `ubuntu`.

### Submit, query, and wait

**1. Check the local device and target.** Run these on the initiating device:

```bash
xrun status --json
xrun ubuntu info --json
```

Use `status` for discovery and local state. Even a successful exit may contain a non-null `server_error`; resolve that discovery error first. Confirm that `info` returns `online: true` before submitting. See `xrun doc quickstart` for initial setup and `xrun doc relay` for automated Cloudflare deployment.

**2. Submit a job.** Generate and save a unique `request_id` for each new job and reuse it for retries. This UUID is only an example:

```bash
xrun ubuntu start --json --request-id 9cb402ae-47fa-4a58-94ac-e84ffac87ca9 -- echo xrun-ok
```

Exit code 0 means the job was accepted. stdout is a Job object, for example (key fields only):

```json
{
  "job_id": "ABC123",
  "request_id": "9cb402ae-47fa-4a58-94ac-e84ffac87ca9",
  "state": "accepted"
}
```

**3. Query or wait using the returned `job_id`.** Replace ABC123 with the actual value:

```bash
xrun ubuntu jobs ABC123 --json
xrun ubuntu wait ABC123 --timeout 60 --json
```

`jobs <ID>` returns a Job object, `jobs --request-id <ID>` returns an array of Jobs, and `wait` returns an object containing `job`, `logs`, and `logs_error`. After success, `wait` stdout looks like this (key fields only):

```json
{
  "job": {
    "job_id": "ABC123",
    "state": "succeeded",
    "result": { "exit_code": 0, "signal": null }
  },
  "logs": [
    { "seq": 1, "stream": "stdout", "data_base64": "eHJ1bi1vawo=" }
  ],
  "logs_error": null
}
```

The Base64 log data decodes to `xrun-ok\n`. A successful `start` still requires checking the final `job.state` and `job.result`. The exit code of `wait` reflects the job result. `logs_error` reports a log-reading problem and does not change a confirmed job result.

### What to do after failure

For example, when `wait --timeout 60 --json` times out, it exits with 75, leaves stdout empty, and writes this to stderr:

```json
{
  "type": "error",
  "code": "WAIT_TIMEOUT",
  "message": "waiting for task timed out; query jobs/logs for its result"
}
```

A wait timeout does not cancel or restart the job. Choose the next step based on the operation:

| Situation | Next step |
| --- | --- |
| `start` returns 75 before a `job_id` is known | Query `jobs --request-id <ID> --json` with the original `request_id`, then query or wait for the matching Job |
| `wait` returns 75 without a confirmed terminal Job | Query or wait for the same Job again |
| Confirmed `failed`, `timed_out`, or `canceled` | Read the result and logs, then decide whether to submit a new job with a new `request_id` |
| `lost` or `DB_RESET` | The result is unknown; verify the actual outcome before deciding whether to execute again |
| Interrupted file transfer | Use `xrun <device> resume <request_id>`; see `xrun doc files` |

First check whether stdout already contains a terminal Job. A remote program can itself exit with 75; in that case, use the confirmed job result. Not finding a Job does not prove the operation never ran. To resume the same command submission, keep the original target, arguments, input, and `request_id`; see `xrun doc errors`. A failed query is not a reason to rerun with a new ID.

Read stdout and stderr separately and interpret exit codes, structured fields, and error codes. stderr may contain multiple diagnostics or progress events; some argument errors are still plain text, so do not parse all of stderr as a single JSON object. Installer scripts use an `ok` field, while the CLI commands above directly return their own data structures.

## Files and screenshots

push and pull transfer one file at a time, with arguments in source–destination order:

```bash
xrun ubuntu pull /home/user/demo/config.json ./config.json --json
xrun ubuntu push ./config.json /home/user/demo/config.json --expect '<sha256 returned by pull>'
xrun ubuntu push ./result.txt /home/user/demo/out/result.txt --mkdir
xrun ubuntu pull /repo/config.json -               # Write to stdout
printf 'hello\n' | xrun ubuntu push - /repo/note.txt
```

- Bytes are transferred unchanged, without encoding, BOM, or newline conversion. Transfers use 64 KiB chunks with no fixed total file size limit. The destination is replaced atomically only after complete reception and SHA-256 verification. Interrupted transfers leave existing files intact.
- Remote relative paths use `-C` or the target's default directory; local relative paths use the current directory. `--mkdir` creates remote parent directories. If pull omits its local path, it saves to a temporary file and prints the absolute path to stdout.
- `--expect <sha256>` overwrites only if the remote file still has that hash; otherwise it returns `STALE`. `--no-overwrite` requires the destination not to exist, otherwise returning `ALREADY_EXISTS`. These options are mutually exclusive.
- Uploading content identical to the remote file skips data transfer but still records an operation.
- pull refuses to write through a local symbolic link; push follows remote symbolic links.
- After interruption, the CLI suggests `xrun <device> resume <request_id>`. Within 24 hours, the transfer can continue from the copy saved at the first attempt. Changes to the network, database, identity, or content digest prevent resumption. If the source file has changed, start a new push. A lost push response is not automatically retried; pull first to verify the destination.
- To transfer directories, combine tar with streaming execution; see `xrun doc streaming`. List a directory with a remote command such as `xrun ubuntu -- ls -la /repo`.

Screenshots capture the target's primary display:

```bash
xrun windows screenshot ./screen.png
xrun windows screenshot ./screen.jpg --format jpeg --max-width 1600 --quality 80
```

The default is a full-resolution PNG. `--format` accepts `png`, `jpeg`, or `webp`; `--quality` (1–100) applies to JPEG and WebP only; `--max-width` scales down but never up. If the path is omitted, the image is saved to a temporary file and its path is printed to stdout, ready for an AI agent to inspect.

| Platform | Requirements |
| --- | --- |
| macOS | The service must run in a logged-in user session with Screen Recording permission; otherwise `PERMISSION_DENIED` |
| Windows | The service must run on an interactive desktop; a locked screen returns `SCREEN_LOCKED` |
| Linux | X11 only; Wayland returns `SCREENSHOT_UNAVAILABLE`, and no graphical session returns `NO_DISPLAY` |

Web screenshots do not require a desktop: run the project's own headless browser script remotely and pull the image back. The target retains attachment copies of transferred files and screenshots; see `xrun doc desktop`.

## Streaming execution

`-i` streams stdin, stdout, and stderr in real time. Use it for large inputs or local pipelines without the 1 MiB input limit:

```bash
xrun ubuntu -i -- cat < ./large.bin > ./copy.bin
xrun ubuntu -i -C /repo -- tar -cf - artifacts > ./artifacts.tar
```

- No terminal (PTY) is provided, so interactive shells and full-screen programs are unsupported.
- Local stdin EOF closes remote stdin; output continues to stream back. The default timeout is 1800 seconds.
- Ctrl+C (exit code 130) or disconnection terminates the remote process without automatic reruns. Use regular execution or `start` if the job must survive disconnection.
- A job record is created and supports `jobs`, `wait`, and `kill`, but output is not stored. `logs` returns `LOG_UNAVAILABLE`.
- Cannot be combined with `start`, `--json`, `--stdin`, `--script`, or `--request-id`.

## Port forwarding

`forward` maps a remote loopback port to a local loopback port, for example to open a remote development server in your browser:

```bash
xrun ubuntu forward 8080:3000       # Local 127.0.0.1:8080 → ubuntu's 127.0.0.1:3000
xrun ubuntu forward 3000            # Same port on both ends
xrun ubuntu forward 0:3000 --json   # Let the OS choose a local port; JSON returns local_address
```

- The local listener binds only to `127.0.0.1`. The target connects only to its own `127.0.0.1` (trying `::1` on failure), never other hosts on its network.
- The command stays in the foreground. Ctrl+C closes the listener and all connections. Interrupted connections are not restored; new connections can reconnect.
- Each TCP connection uses one relay session, with up to 32 connections per forward. Cloudflare relays allow up to 8 sessions per network.
- Other local processes can also access the port. Forwarded connections close when the target pauses access, denies the source, or receives its revocation.

## Desktop app

The macOS and Windows desktop app manages networks, permissions, background services, and local activity. It can also run commands, transfer files, and take screenshots on other devices. See `xrun doc install` for installation. Only one app instance runs at a time; closing the window leaves it in the menu bar or system tray.

**Connect devices.** Before joining a network, the app shows **Connect your devices**:

- **Access code**: enter the relay address and access code, or paste a short link.
- **Use a link**: paste a deployment link or member invitation; the app detects whether to create or join a network.
- **New network**: click **Authorize and create**. After browser authorization with Cloudflare, the app deploys the relay, creates the network, and starts the service. **Relay options** lets you change the relay name and region; choose an account first if several are available. If the relay was deployed but network creation failed, retry reuses that relay.

When joining, you can select **Allow the manager to access this device**. Leaving it unchecked explicitly denies access even if the invitation requests it. The device name defaults to the hostname. An existing CLI identity is reused.

**Interface.** The sidebar contains Devices, Activity, Settings, and a local device card showing name, role, state, and a pause-access button. Click the card to open network settings. If system approval is needed, the service is stopped, or remote access is paused, a banner provides the appropriate action.

- **Devices**: lists other network devices with search and online-state filters. Each device shows its OS, architecture, and short ID. Its access switch controls whether it may access this machine. For online devices that allow your device, you can run commands, transfer files, and capture screenshots. The device menu shows the ID, version, last-seen time, and last connection type (direct or relayed); the manager can also revoke members. Revoked members are collapsed separately. The manager can generate access codes and copy a connection command or short link from **Invite device**. Today's traffic appears at the bottom; **Details** shows trends and per-device statistics.
- **Run command**: enter the command, interpreter, remote working directory, and timeout; view output, state, and exit code, or cancel the job. Closing the dialog does not affect an accepted job. Recent submissions retains up to 20 entries from the last 24 hours; uncertain results can be queried without executing again.
- **Transfer files**: upload or download a single file, create directories, and allow overwrites. Interrupted transfers can be resumed or abandoned.
- **Screenshot**: choose original PNG, a bandwidth-saving preset, or a custom format, maximum width, and quality.
- **Activity**: shows all operations other devices performed on this machine, grouped by date, with running/failed filters and search within the current page. Details include working directory, duration, exit result, and output. File and screenshot attachments can be previewed (PNG/JPEG up to 8 MiB, text up to 1 MiB) or saved elsewhere. Activity reads the local database and remains available with the service stopped or network disconnected.
- **Settings**:
  - General: language (system, Chinese, English), automatic lightweight mode and delay, LAN direct connections, compression, showing the xrun icon at login, and hiding the menu bar/tray icon.
  - Activity and attachments: attachment retention, 30 days by default; 0 means indefinite.
  - Execution environment: default working directory, concurrent jobs, and PATH. Changes affect new jobs.
  - Network and service: network information, all-member access, service start/stop, roster lease (manager only), locally deployed relays (account authorization, remembered login, deployment link), manager identity backup/handover/restore, and background service removal.
  - About: version, system, and data directory.

**Menu bar and tray.** The menu shows local state, links to open xrun, Activity, Settings, and Lightweight mode, and a primary action based on current state: join a network, start the service, pause access, or resume access. Double-clicking the Windows tray icon also opens the window. If you hide the icon, reopening the app restores it.

**Lightweight mode.** Background startup creates no webview. By default, closing the window releases the window and webview after 30 seconds, keeping the native tray host and independent daemon running. Change or disable the delay in General settings, or enter manually from the tray or Settings. Reopening cancels the timer or recreates the interface on the last page. Authorization, submissions, confirmation dialogs, file dialogs, and unsaved execution or attachment-retention settings defer release; save those settings after reopening to allow it. Other temporary dialogs and inputs reset when the interface is recreated. Device connections and accepted background jobs continue running.

**Background service.** A separate service handles execution and transfers:

- Quitting xrun closes the interface while the service keeps running.
- Stopping the service (Settings → Network and service) shuts it down normally and cancels jobs running locally. The service starts again at the next login.
- Removing the background service disables automatic startup while preserving local data.
- macOS registers the service through system login items. You may need to allow xrun in System Settings → General → Login Items. Screenshots require Screen Recording permission. After replacing the app, the next service start registers the current executable again.
- On Windows, the service runs in the current user's login session without a console window.
- To switch from a CLI-installed service to the app, run `xrun daemon stop`, then start the service in the app. The app replaces the service registration.

## Configuration and services

`connect`, `up`, and `join` install a background service by default, running as the current user: a systemd user service with linger on Linux, a LaunchAgent on macOS, or a scheduled task at login on Windows. The desktop app registers its own service; see `xrun doc desktop`.

```bash
xrun daemon install      # Register and start the service; requires an existing device identity
xrun daemon start
xrun daemon stop         # Stop normally and cancel jobs running locally
xrun daemon uninstall    # Remove registration, keep data
xrun daemon              # Run in the foreground
xrun daemon reset        # Rebuild the job database while stopped, keeping the device identity
```

For Linux containers without a systemd user session, join with `--no-daemon`, then run `xrun daemon` under an external process manager. In `status`, `daemon_installed` only indicates service registration. Check `daemon_running` and `daemon_connected` as well.

Configuration, identities, and databases are stored in `~/.xrun` (`%USERPROFILE%\.xrun` on Windows), accessible only to the current user. Execution settings and permissions are stored in `daemon.toml`:

```toml
allow_from = ["b63c4960932e4b46afdb8028e9490cc8"]
default_cwd = "/home/user/repo"
max_concurrent_jobs = 4
attachment_retention_days = 30

[env]
PATH = "/home/user/.cargo/bin:/home/user/.local/bin:/usr/local/bin:/usr/bin:/bin"
```

- Changes affect new jobs without restarting the service. Saving settings through xrun preserves other fields. `~` in paths is not expanded; use the target user's actual home path. Windows PATH entries use semicolons.
- Background services usually do not load interactive shell configuration. Add toolchain directories such as `~/.cargo/bin` to PATH in `[env]`.
- Jobs inherit the service environment except these build variables: `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET`, `RUSTUP_TOOLCHAIN`, `RUST_RECURSION_COUNT`, `RUSTC`, `RUSTDOC`, `RUSTC_WRAPPER`, `RUSTC_WORKSPACE_WRAPPER`, `RUSTFLAGS`, and `CARGO_ENCODED_RUSTFLAGS`. Set them in `[env]` or pass `--env` when needed.
- `max_concurrent_jobs` limits concurrent commands to 1–64.
- The target keeps independent attachment copies of transferred files and screenshots. `attachment_retention_days` defaults to 30, accepts 0 for indefinite retention, and has a maximum of 3650. Expired copies are cleaned every 10 minutes; activity summaries remain.

Service logs are separate from job logs. CLI-installed services on macOS write to `~/.xrun/daemon-service.log`. On Linux, use `journalctl --user -u xrun-daemon.service`. For other setups, consult your external process manager's logs.

## Status and troubleshooting

```bash
xrun status --json          # Local service, permissions, and connected devices
xrun ubuntu info --json     # Connect to the target and read OS, version, hostname, and default directory
xrun recent --json          # Recent local submissions
xrun traffic --period today
```

Online state in `status` comes from the relay and only means the device is connected to it; it does not guarantee command execution. Use `info` to check target availability. A device that suddenly loses connectivity typically appears offline after 45–60 seconds.

`--json` writes data to stdout and diagnostics to stderr. Foreground execution does not support `--json` and forwards remote output unchanged.

### Exit codes

| Exit code | Meaning |
| --- | --- |
| 0–255 | Remote program exit code (values above 255 on Windows map to 1; the full value remains in the job result) |
| 128+N | Remote process terminated by signal N |
| 124 | Execution timed out; the process has stopped |
| 130 | Ctrl+C: cancellation confirmed or request not yet accepted |
| 75 | **Unconfirmed result**: the request may have arrived; query before deciding whether to retry |
| 125 | Connection, identity, permission, or execution-layer error |
| 2 | Local argument error |
| 1 | Confirmed failure of an operation such as a file transfer or screenshot |

Remote programs may also return these values. Interpret them alongside `[xrun]` diagnostics on stderr, error codes, and job state.

### Unconfirmed result (75)

Query first instead of immediately running again:

```bash
xrun recent                                   # Find the request_id
xrun windows jobs --request-id '<request_id>'
xrun windows wait <job-id>
```

To resume the same submission, rerun with the original arguments, input, and `--request-id`. The target deduplicates by source and request ID, so an accepted request does not run twice. A normal invocation always generates a new request_id. If the target's job database has been rebuilt, it returns `DB_RESET`.

Ctrl+C tries to cancel a reliable job and returns 75 if cancellation cannot be confirmed. SIGTERM and SIGHUP do not cancel submitted jobs.

### Traffic statistics

`xrun traffic` reads relay traffic counters without requiring the local service to run. The manager can view network totals and device details; other members can only view themselves. `--period` accepts `today`, `month` (default), or `all`, based on UTC dates. Paginate device details with `--limit` (1–256) and `--offset`; JSON `next_offset` points to the next page.

Counters cover encrypted data between devices, including TLS handshakes and protocol overhead, but exclude relay authentication, acknowledgments, and heartbeats. Inbound means device-to-relay; outbound means relay-to-device. Use outbound alone for forwarded volume rather than adding inbound and outbound. Statistics accumulate from relay startup, do not merge across relay replacements, and exclude direct connections. Recording errors report `complete: false`. These figures are not a billing statement. xrun does not impose traffic quotas or rate limits.

### Common error codes

| Error code | Action |
| --- | --- |
| `SOURCE_NOT_ALLOWED` | Run `allow-from <source>` on the target |
| `ACCESS_PAUSED` | Access is paused on the target; run `xrun daemon resume` there |
| `ROSTER_EXPIRED` | The manager has been offline past lease expiry; start its service |
| `DEVICE_OFFLINE` | Check the target service, relay, and network; xrun does not queue execution |
| `DEVICE_BUSY` | The target's command slots are full; retry later with the original request_id |
| `DEVICE_REVOKED` | The target or local device was revoked |
| `INVALID_CODE`, `CODE_EXPIRED`, `CODE_LOCKED` | Check the relay address and code; generate a new code if expired or attempts are exhausted |
| `CODE_IN_USE`, `PAIRING_BUSY` | Another device is using the code; generate a new one for this device or retry later |
| `RELAY_CLAIMED` | The relay belongs to another network; ask its manager for an invitation |
| `RELAY_UNCLAIMED` | The relay has no network yet; create one on the manager or start its service |
| `NOT_MANAGER`, `MANAGER_PROTECTED` | The operation requires the manager, or the manager rejects all-member trust |
| `VERSION_MISMATCH` | Local CLI/app/service versions differ, or device protocols are incompatible; update consistently and restart services |
| `PROGRAM_NOT_FOUND`, `SHELL_REQUIRED` | Check the target's PATH or use an absolute path; use `--script cmd` for batch files |
| `INVALID_CWD` | `-C` must name an existing absolute directory on the target |
| `STALE`, `ALREADY_EXISTS` | The remote file changed or already exists; pull and inspect it first |
| `PERMISSION_DENIED`, `SCREEN_LOCKED`, `NO_DISPLAY` | Missing screenshot permission, a locked screen, or no graphical session |
| `INTERACTIVE_REQUIRED` | Use an interactive terminal or provide options such as `--account` or `--password-stdin` |
| `DB_RESET` | The target's job database was rebuilt; old jobs cannot be queried or replayed |
| `DB_SCHEMA_MISMATCH` | The database format does not match the program version; existing data is untouched. See `xrun doc upgrade`. |

## Upgrading and removal

xrun is still a prerelease (`-rc`). Protocol and data compatibility between RC versions is not guaranteed. Before continuing operations after an upgrade, update the CLI, app, background service, and relay across all devices to the same RC version.

The CLI, app, and background service on one device must have the same version. Restart the service after replacing executables. Protocol, signature format, and database schema numbers are currently all 1; a shared number does not mean different RCs can be mixed. Stable-release compatibility and upgrade policies will be defined at the stable release.

```bash
xrun daemon stop       # Cancels running jobs; let them finish first
# Replace the executable or run the new version's installer
xrun --version
xrun daemon start
xrun status
```

Installer scripts stop the service and replace executables automatically. When an updated desktop app opens, it stops a mismatched service and restarts it with the new version. A service that was already stopped remains stopped.

Remove services and data:

```bash
xrun down            # Remove local services, keep data
xrun down --purge    # Delete the local identity and all data after interactive confirmation
```

- On Linux, `down` also removes the local relay service. For an app-managed service, use the app's background service removal action.
- On the manager, `--purge` deletes the network root private key, preventing further member issuance or revocation. Unless a backup can restore it, create a new network and have every member rejoin and grant access again.
- After a member runs `--purge`, it can only rejoin as a new device. The manager should revoke its old identity.
- To clear job records only, stop the service and run `xrun daemon reset`. The device identity remains, and old job references cannot be used with the new database.
- For `DB_SCHEMA_MISMATCH`, do not delete identity files or `roster.db`; the latter stores the latest revocation state.

To uninstall: on macOS, remove the background service, then delete `xrun.app` and `~/.local/bin/xrun`; on Windows, uninstall from Apps in system settings; on Linux, delete the `xrun` executable.
