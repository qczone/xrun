<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logos/xrun-wordmark-dark.svg">
    <img src="assets/logos/xrun-wordmark-light.svg" alt="xrun" width="203" height="66">
  </picture>
</p>

<p align="center">
  <strong>Run programs, manage jobs, and transfer files across your devices.</strong>
</p>

<p align="center">
  <a href="docs/usage.md#installation">Install</a> ·
  <a href="docs/usage.md#quick-start">Quick start</a> ·
  <a href="docs/usage.md">User manual</a> ·
  <a href="docs/development.md">Development</a>
</p>

<p align="center">English · <a href="README.zh-CN.md">简体中文</a></p>

xrun is a tool for developers and AI agents working across their own macOS, Windows, and Linux devices. Use the same commands to build, test, run background jobs, transfer files, capture screenshots, and forward ports. The desktop app manages networks, permissions, background services, and activity, with English and Chinese interfaces and light and dark themes.

```bash
xrun ubuntu -C /home/user/demo -- cargo test       # Run tests on Linux
xrun windows start -C 'D:\demo' -- cargo build     # Start a background build on Windows
xrun windows pull 'D:\demo\target\debug\app.exe' ./app.exe
xrun windows screenshot ./screen.png
xrun ubuntu forward 8080:3000                      # Open a remote dev server locally
```

## What you can do

- **Cross-platform execution**: run programs or scripts on a target device and stream their output and exit codes back.
- **Background jobs**: check status, read logs, wait, or cancel after submission. Jobs keep running if you disconnect.
- **Files and screenshots**: transfer individual files with checksums, resume support, and overwrite protection; capture remote displays.
- **Port forwarding**: open a remote development service in your local browser.
- **AI automation**: defined exit codes, error codes, and JSON output. `xrun help` and `xrun doc` work offline.

PTY terminals, directory synchronization, and offline job queues are not supported.

## Quick start

This example uses a Mac named `macbook` to manage a Linux device named `ubuntu`. Install xrun on both devices first.

1. Download your platform's package from [Releases](https://github.com/xrun-sh/xrun/releases), or use the [command-line installer](docs/usage.md#command-line-installation).
2. On the Mac, deploy a relay, then create a network using the complete address it returns. The Mac becomes the manager:

   ```bash
   xrun relay deploy --provider cloudflare --name xrun-relay
   xrun connect '<returned relay address>' --name macbook
   ```

3. Run `xrun invite` on the Mac to generate an access code. On Linux, join the network and allow the Mac to access this device:

   ```bash
   xrun connect <relay-address> <access-code> --name ubuntu --allow-manager
   ```

4. Back on the Mac, check the connection and run your first remote command:

   ```bash
   xrun status
   xrun ubuntu -- uname -s
   ```

   If the command prints `Linux` and exits with code 0, remote execution is working. See [Status and troubleshooting](docs/usage.md#status-and-troubleshooting) if it fails.

The desktop app's **New network** flow can deploy a relay and create a network for you. You can also host a relay on Linux; see [Relay deployment](docs/usage.md#relay-deployment).

## Platforms

| Platform | Download |
| --- | --- |
| macOS 13+ · x86_64 / arm64 | App ZIP with CLI |
| Windows · x86_64 / arm64 | Installer EXE with app and CLI |
| Linux · x86_64 / arm64 | CLI archive |

Deploy a relay to Cloudflare (Worker + Durable Object) with one command from any supported system, or host it on Linux.

## Security

- **End-to-end encryption**: devices use mutual TLS inside the relay tunnel. The relay stores network ownership, the manager-signed roster (device names, key fingerprints, and revocations), and traffic counters. It cannot read commands, output, files, or screenshots.
- **Membership does not grant access**: each device decides who may access it. An allowed device has all the permissions of the current local user. Only allow devices you trust.
- **Keep the manager online**: it handles invitations, revocations, and roster lease renewal (15 minutes by default, configurable up to 24 hours). Once the lease expires while the manager is offline, new remote operations are rejected. Accepted jobs keep running.

## Documentation

- [User manual](docs/usage.md): installation, networking, permissions, everyday use, and troubleshooting. Read it offline with `xrun doc`, or use `xrun doc --lang zh` for Chinese. AI agents and scripts can start with [`xrun doc automation`](docs/usage.md#automation-and-ai).
- [Development and releases](docs/development.md): builds, tests, release workflow, architecture, protocol, and compatibility rules.

[MIT License](LICENSE)
