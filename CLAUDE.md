# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

nylon-run (`nyrun`) — a language-agnostic process manager and reverse proxy. Built in Rust on Pingora (Cloudflare's proxy framework) with SlateDB for persistent state. Supports containerless OCI execution — pull any Docker/OCI image, extract it, and run the binary directly as a native host process.

## Build & Run Commands

- **Build:** `cargo build`
- **Run:** `cargo run`
- **Test:** `cargo test`
- **Integration test:** `docker build -t nyrun-test -f tests/Dockerfile . && docker run --rm nyrun-test`
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
nyrun run nginx:latest --p 80:80                  # short OCI name (defaults to docker.io/library/)
nyrun run traefik:v3.6 --p 80:80                  # short OCI name with tag
nyrun run ghcr.io/xx/xx:latest --p 8081:8081     # full OCI reference, containerless execution
nyrun run ghcr.io/xx/xx:latest --allow all --p 8081:8081  # OCI but allow full filesystem access
nyrun start ecosystem.yaml                        # start all apps from k8s-style config file
nyrun start ecosystem.yaml --only api             # start only one app from config
nyrun export -o ecosystem.yaml                    # export running processes as YAML
nyrun ls                                          # list all managed processes with status
nyrun del <name>                                  # stop and remove a process
nyrun restart <name>                              # restart a process
nyrun reload <name>                               # graceful reload (zero-downtime)
nyrun update <name> [--p ...] [--ssl ...] [...]   # update process config without removing
nyrun update <name> --image ghcr.io/xx/xx:v2     # update OCI image (re-pull + restart)
nyrun logs <name>                                 # tail logs for a process
nyrun logs <name> --lines 100                    # last N lines
nyrun metrics enable                              # start Prometheus metrics on :9090
nyrun metrics enable --port 9100                  # start metrics on custom port
nyrun metrics disable                             # stop metrics server
nyrun set default-registry docker.io              # set default OCI registry
nyrun set cache-ttl 120                           # set proxy cache TTL in seconds
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
  - When `public_port == app_port` (e.g. `--p 80:80`), auto-assigns a free internal port and injects `PORT` env var
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
  - Accepts local paths, short OCI names (`nginx:latest`), or full OCI references (`ghcr.io/org/image:tag`)
  - Local binaries are copied to `/var/run/nyrun/apps/<name>/` before execution
  - OCI images are extracted to `/var/run/nyrun/oci/<name>/` and run as native processes (containerless)
- `start` subcommand — start processes from a YAML config file (k8s-style manifests)
  - Multi-document YAML with `---` separators
  - Supports `kind: Process` and `kind: ConfigMap`
  - `--only NAME` — start only a specific process from the config
  - If `port` is present in spec → proxy mode; otherwise → process-only mode
  - Supports `volumes` for mounting host files/dirs or ConfigMaps into process working directory
  - Config format:
    ```yaml
    kind: ConfigMap
    metadata:
      name: app-config
    data:
      config.yaml: |
        key: value
    ---
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
      acme: user@mail.com
      deny: net,io
      allow: /tmp
      volumes:
        - configmap:app-config:/etc/app
        - ./local-file.conf:/etc/app/app.conf
    ```
- `metrics` subcommand — opt-in Prometheus metrics server
  - `nyrun metrics enable` — start metrics on port 9090 (default)
  - `nyrun metrics enable --port 9100` — custom port
  - `nyrun metrics disable` — stop metrics server
  - Metrics are NOT started automatically with the daemon
- `update` subcommand — `--image ghcr.io/xx/xx:v2` re-pulls OCI image and restarts
- `backup` / `restore` — zip/unzip the entire `/var/run/nyrun/` working directory

### Working Directory

All runtime data lives under `/var/run/nyrun/`:
- `/var/run/nyrun/apps/<name>/` — local binary copies
- `/var/run/nyrun/oci/<name>/` — extracted OCI images
- `/var/run/nyrun/configmaps/<name>/` — ConfigMap data files
- `/var/run/nyrun/logs/` — per-process log files
- `/var/run/nyrun/certs/` — ACME certificates
- `/var/run/nyrun/state/` — SlateDB persistent state
- `/var/run/nyrun/settings.json` — global settings (default-registry, etc.)

## Architecture Overview

### Core Components

1. **Daemon** — nyrun runs as a background daemon process (auto-starts on first command)
   - Daemon spawns automatically when any command is issued (no explicit start)
   - PID file at `/var/run/nyrun/nyrun.pid`
   - All commands communicate with the daemon via Unix socket

2. **Process Manager** — spawn, monitor, restart, and manage processes
   - Local binaries are copied to `/var/run/nyrun/apps/<name>/` and run from their own isolated directory
   - Auto-restart on crash, log capture (stdout/stderr)
   - **eBPF Sandboxing (Linux only)** — kernel-level process isolation without containers
     - `--deny net` blocks socket syscalls via seccomp-BPF
     - `--deny io` restricts filesystem access via Landlock LSM
     - `--allow` whitelists specific paths when `io` is denied

3. **Reverse Proxy (Pingora)** — activated by `--p` flag
   - Port mapping with auto-remap when public == app port
   - SPA static file serving with `--spa` flag
   - **TLS/SSL** with SNI-based dynamic certificate selection
   - **Auto SSL (ACME)** — automatic Let's Encrypt certificates via HTTP-01 challenge
   - **In-memory caching (moka):**
     - `Cache<String, (ResponseHeader, Bytes)>` — 10,000 entries, 60s TTL
     - Only caches GET requests with 200 OK responses
     - `X-Cache: HIT` header on cache hits

4. **Persistent State (SlateDB)** — embedded KV store under `/var/run/nyrun/state/`

5. **Native OCI Execution** — containerless: pull images from OCI registries, extract to `/var/run/nyrun/oci/<name>/`, run as native host processes
   - Supports short names (`nginx:latest`) with configurable default registry
   - OCI processes are sandboxed to their own folder via eBPF by default
   - `--allow all` to disable isolation
   - Per-layer progress logging during pull

6. **ConfigMap** — inline configuration data in YAML manifests (k8s-style)
   - Written to `/var/run/nyrun/configmaps/<name>/`
   - Mounted into processes via `volumes: [configmap:<name>:/dest/path]`

7. **Observability (Prometheus)** — opt-in via `nyrun metrics enable`
   - Process metrics: restart count, CPU/memory usage per process
   - Proxy metrics: request count, latency histograms, active connections
   - Cache metrics: hit/miss ratio
   - System metrics: managed processes, OCI pull stats

8. **Cloud Agent** (cloud UI/server is a separate private project)
   - `nyrun link <api-key>` / `nyrun unlink`
   - Persistent outbound WebSocket connection
   - Heartbeat + reconnect with exponential backoff

### Key Design Decisions

- **Single `run` command** — no separate `bin` subcommand; `--p` is optional
- **Pingora** for proxy — battle-tested at Cloudflare, async, high performance
- **SlateDB** for persistence — embedded LSM-tree KV, no external DB
- **moka** for in-memory caching — async-ready, TTL-based eviction
- **`/var/run/nyrun/`** as the single working directory — simple backup/restore model
- **Containerless OCI** — pull and extract, execute natively on host, eBPF sandbox by default
- **K8s-style config** — multi-document YAML with `kind`, `metadata`, `spec` structure
- **Metrics opt-in** — not started automatically, enable via `nyrun metrics enable`

### Expected Dependencies

| Crate | Purpose |
|-------|---------|
| `pingora` / `pingora-proxy` | HTTP proxy framework |
| `slatedb` | Embedded persistent KV store |
| `moka` | In-memory cache |
| `tokio` | Async runtime |
| `serde` / `serde_json` / `serde_yaml` | Serialization (JSON + YAML) |
| `oci-distribution` | OCI image pulling |
| `clap` | CLI argument parsing |
| `openssl` | TLS/SSL certificate handling |
| `instant-acme` | ACME / Let's Encrypt client |
| `prometheus-client` | Metrics exposition |
| `zip` | Backup/restore |
| `landlock` | Filesystem sandboxing (Linux) |
| `seccompiler` | Network sandboxing (Linux) |
| `mimalloc` | Memory allocator |
