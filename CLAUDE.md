# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

nylon-run (`nyrun`) — a Rust process manager and reverse proxy, similar to PM2 but language-agnostic. Built on Pingora (Cloudflare's proxy framework) with SlateDB for persistent state. No config files — everything is driven by CLI commands.

## Build & Run Commands

- **Build:** `cargo build`
- **Run:** `cargo run`
- **Test:** `cargo test`
- **Run single test:** `cargo test <test_name>`
- **Check (fast compile check):** `cargo check`
- **Lint:** `cargo clippy`
- **Format:** `cargo fmt`

## CLI Usage

```
nyrun run ./xxx                                  # process management only, no proxy
nyrun run ./xxx --args "--port 8000 --verbose"   # pass args to the binary
nyrun run ./xxx --env-file .env                   # env file with process-only mode
nyrun run ./xxx --p 80:8000                      # proxy (public:internal) + process management
nyrun run ./xxx --p 80:8000 --args "--config app.yaml"  # proxy + args to binary
nyrun run ./xxx --p domain.com:80:8000           # proxy with host-based routing on shared port
nyrun run ./yyy --p other.com:80:9000            # multiple services sharing port 80 via different hosts
nyrun run ./html_folder --spa --p 8080           # serve static files as SPA
nyrun run ./xxx --p domain.com:443:8000 --ssl cert.pem key.pem  # HTTPS with manual certs
nyrun run ./xxx --p domain.com:443:8000 --acme user@mail.com   # HTTPS with auto Let's Encrypt
nyrun run ./xxx --env-file .env --p 80:8000      # load env vars from file
nyrun run ./xxx --deny net,io --p 80:8000         # sandbox: deny network & disk I/O (Linux eBPF)
nyrun run ./xxx --deny io --allow /tmp/data,/var/log --p 80:8000  # deny I/O except whitelisted paths
nyrun run ./xxx --deny net                        # process-only with network denied
nyrun run ghcr.io/xx/xx:latest --p 8081:8081     # pull OCI image, isolated to its own folder by default
nyrun run ghcr.io/xx/xx:latest --allow all --p 8081:8081  # OCI but allow full filesystem access
nyrun start ecosystem.json                        # start all apps from config file (like PM2)
nyrun start ecosystem.json --only api             # start only one app from config
nyrun ls                                          # list all managed processes with status
nyrun del <name>                                  # stop and remove a process
nyrun restart <name>                              # restart a process
nyrun reload <name>                               # graceful reload (zero-downtime)
nyrun update <name> [--p ...] [--ssl ...] [...]   # update process config without removing
nyrun update <name> --image ghcr.io/xx/xx:v2     # update OCI image (re-pull + restart)
nyrun logs <name>                                 # tail logs for a process
nyrun logs <name> --lines 100                    # last N lines
nyrun backup -o output_name                      # zip entire /var/run/nyrun/ as backup
nyrun restore backup.zip                         # restore by extracting zip over /var/run/nyrun/
nyrun save                                        # save current process list for restore on reboot
nyrun startup                                     # generate systemd unit + enable auto-start on boot
nyrun unstartup                                   # remove systemd unit
nyrun kill                                        # stop daemon + all managed processes
nyrun link <api-key>                              # connect this instance to cloud UI
nyrun unlink                                      # disconnect from cloud UI
```

### Key CLI Concepts

- `run` subcommand — manage a process (with optional reverse proxy via `--p`)
  - Without `--p`: process management only (no proxy)
  - With `--p`: enables Pingora reverse proxy
  - `--p` port mapping formats:
    - `--p PUBLIC_PORT:APP_PORT` — simple port mapping
    - `--p HOST:PUBLIC_PORT:APP_PORT` — host-based routing (multiple services can share the same public port with different hostnames)
    - `--p PORT` — single port, listen only (e.g. SPA static serving)
  - `--spa` — serve a directory as a single-page application (fallback to index.html)
  - `--ssl CERT_PATH KEY_PATH` — enable TLS with manual certs
  - `--acme EMAIL` — auto SSL via Let's Encrypt ACME (HTTP-01 challenge), certs stored in `/var/run/nyrun/certs/`
  - `--env-file PATH` — load environment variables from a dotenv file, passed to the spawned process
  - `--deny CAPS` — sandbox the process using eBPF (Linux only). Comma-separated capabilities to deny:
    - `net` — block network access (outbound/inbound)
    - `io` — block filesystem I/O (read/write outside working dir)
  - `--allow PATHS` — whitelist comma-separated directories when using `--deny io` (e.g. `--allow /tmp/data,/var/log`)
  - `--args "ARGS"` — pass arguments to the spawned binary (quoted string)
  - Accepts local paths (binary or directory) or OCI image references (e.g. `ghcr.io/org/image:tag`)
- `start` subcommand — start processes from a YAML config file (k8s-style manifests)
  - Multi-document YAML with `---` separators, each document is a `kind: Process` manifest
  - `--only NAME` — start only a specific process from the config
  - If `port` is present in spec → `run` mode; otherwise → `bin` mode
  - Supports `volumes` for mounting host files/dirs into process working directory
  - Config format:
    ```yaml
    kind: Process
    metadata:
      name: api
    spec:
      path: ./api-server
      port: "domain.com:443:8000"
      args: "--verbose"
      env_file: .env
      env:
        NODE_ENV: production
      ssl: [cert.pem, key.pem]
      acme: user@mail.com
      deny: net,io
      allow: /tmp
      volumes:
        - ./config.yaml:/etc/app/config.yaml
    ```
- `update` subcommand — `--image ghcr.io/xx/xx:v2` re-pulls OCI image and restarts
- `backup` / `restore` — zip/unzip the entire `/var/run/nyrun/` working directory

### Working Directory

All runtime data (extracted binaries, OCI layers, state, logs) lives under `/var/run/nyrun/`. Backup/restore operates on this directory as a whole — zip it out, zip it back in.

## Architecture Overview

### Core Components

1. **Daemon** — nyrun runs as a background daemon process (auto-starts on first command)
   - Daemon spawns automatically when any command is issued (no explicit start)
   - PID file at `/var/run/nyrun/nyrun.pid`
   - `nyrun save` — snapshot current process list to disk for restore on reboot
   - `nyrun startup` — generate + enable systemd unit (Linux) so daemon + saved processes auto-restore on boot
   - `nyrun unstartup` — remove systemd unit
   - `nyrun kill` — stop daemon + all managed processes
   - All `bin`/`run`/`ls`/`del`/etc. commands communicate with the daemon via Unix socket

2. **Process Manager** — spawn, monitor, restart, and manage binary processes
   - Run arbitrary binaries with custom environment variables
   - Process lifecycle: start, stop, restart, delete
   - Auto-restart on crash
   - Log capture (stdout/stderr) per process
   - **eBPF Sandboxing (Linux only)** — restrict process capabilities at kernel level
     - `--deny net` attaches eBPF programs to block socket syscalls for the process
     - `--deny io` restricts filesystem access outside the process working directory
     - `--allow` whitelists specific paths when `io` is denied (eBPF checks path prefix)
     - Enforced per-process, no container overhead

3. **Reverse Proxy (Pingora)** — activated by `run` subcommand with `--p`
   - Port mapping: route `HOST_PORT` → `APP_PORT` on the managed process
   - SPA static file serving with `--spa` flag (fallback to index.html)
   - **TLS/SSL** with SNI-based dynamic certificate selection (same pattern as nylon-mesh)
     - Multiple certs per listener — routes to correct cert by SNI hostname
     - Supports default cert fallback when no SNI match
     - Uses Pingora's `TlsAccept` callback with OpenSSL
   - **Auto SSL (ACME)** — automatic Let's Encrypt certificates
     - HTTP-01 challenge: nyrun handles `/.well-known/acme-challenge/` on port 80 automatically
     - Certs auto-issued on first request, stored in `/var/run/nyrun/certs/`
     - Auto-renewal before expiry
     - Only requires `--acme user@email.com` — host derived from `--p HOST:PORT:APP_PORT`
   - **In-memory caching (moka):**
     - `Cache<String, (ResponseHeader, Bytes)>` — 10,000 entries, 60s TTL
     - Cache key: `{host}{uri}{query}`
     - Only caches GET requests with 200 OK responses
     - 5MB max body size per entry (OOM protection)
     - Async cache save via spawned tokio task
     - `X-Cache: HIT` header on cache hits

4. **Persistent State (SlateDB)** — embedded KV store under `/var/run/nyrun/`
   - Stores process definitions and runtime state
   - Survives restarts — processes auto-recover on nyrun startup

5. **OCI Puller** — pull images from OCI registries, extract to `/var/run/nyrun/oci/<name>/`, run natively (no containers)
   - **Isolated by default:** OCI processes are sandboxed to their own folder (`/var/run/nyrun/oci/<name>/`) via eBPF on Linux — no `--deny io` needed
   - `--allow PATHS` to whitelist additional directories, `--allow all` to disable isolation entirely
   - Behaves like a lightweight container without the runtime overhead

6. **Backup/Restore** — zip the entire `/var/run/nyrun/` directory; restore by overwriting it

7. **Logging** — per-process log capture and retrieval
   - stdout/stderr captured to log files under `/var/run/nyrun/logs/`
   - `nyrun logs <name>` to tail, `--lines N` for last N lines
   - Log rotation to prevent unbounded growth

8. **Observability (Prometheus + Grafana)**
   - Built-in Prometheus metrics endpoint (e.g. `/metrics` on a dedicated port)
   - **Process metrics:** uptime, restart count, CPU/memory usage per process
   - **Proxy metrics:** request count, latency histograms, status code distribution, active connections
   - **Cache metrics:** hit/miss ratio, cache size, eviction count
   - **System metrics:** total managed processes, OCI pull stats
   - Ready for Grafana dashboards out of the box

9. **Cloud Agent** (cloud UI/server is a separate private project — not in this repo)
   - `nyrun link <api-key>` / `nyrun unlink` — connect/disconnect to cloud
   - Agent pushes metrics, logs, and status to cloud via WebSocket/gRPC
   - Persistent outbound connection — no inbound ports needed on the agent side
   - Heartbeat + reconnect with exponential backoff
   - Receives and executes cloud → agent commands (restart, reload, del, update)

### Key Design Decisions

- **No config files** — all configuration via CLI flags; state persisted in SlateDB
- **Pingora** for proxy — battle-tested at Cloudflare, async, high performance
- **SlateDB** for persistence — embedded LSM-tree KV, no external DB
- **moka** for in-memory caching — async-ready, TTL-based eviction
- **`/var/run/nyrun/`** as the single working directory — simple backup/restore model
- **OCI for distribution only** — pull and extract, execute natively on host

### Caching

- Tier 1 only (moka in-memory): `Cache<String, (ResponseHeader, Bytes)>`, 10,000 entries, 60s TTL
- Cache save is async (spawned tokio task, non-blocking)
- Shared cache instance across all port listeners via `Cache::clone()` (Arc internally)

### TLS Pattern (from nylon-mesh)

Reference: [`nylon-mesh/src/tls_accept.rs`](https://github.com/AssetsArt/nylon-mesh/blob/main/src/tls_accept.rs)

- `DynamicCertificate` struct holds `HashMap<String, TlsCertificate>` keyed by SNI hostname + optional default cert
- Implements Pingora's `TlsAccept` trait — `certificate_callback` selects cert by SNI at handshake time
- `new_tls_settings()` loads cert/key PEM files and builds `TlsSettings::with_callbacks`
- Chain certs supported for intermediate CA

### Expected Dependencies

| Crate | Purpose |
|-------|---------|
| `pingora` / `pingora-proxy` | HTTP proxy framework |
| `slatedb` | Embedded persistent KV store |
| `moka` | In-memory cache (Tier 1) |
| `tokio` | Async runtime |
| `serde` / `serde_json` | Serialization |
| `oci-distribution` or similar | OCI image pulling |
| `clap` | CLI argument parsing |
| `openssl` | TLS/SSL certificate handling |
| `instant-acme` or `acme2` | ACME / Let's Encrypt client |
| `prometheus` / `prometheus-client` | Metrics exposition |
| `zip` | Backup/restore |
| `aya` or `libbpf-rs` | eBPF sandboxing (Linux only) |
| `mimalloc` | Memory allocator |
