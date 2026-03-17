# nylon-run Implementation Checklist

## Core Infrastructure
- [x] Project setup (Cargo.toml dependencies, module structure)
- [x] CLI parsing with clap (`bin`, `run`, `ls`, `del`, `restart`, `reload`, `update`, `logs`, `save`, `startup`, `unstartup`, `kill`, `backup`, `restore`, `link`, `unlink`)
- [x] SlateDB persistent state (`/var/run/nyrun/state/`) — migrated from JSON file with auto-migration
- [x] Working directory setup (`/var/run/nyrun/`)

## Daemon
- [x] Auto-start daemon on first command, PID file at `/var/run/nyrun/nyrun.pid`
- [x] CLI ↔ daemon communication (Unix socket)
- [x] `nyrun save` — snapshot current process list for restore on reboot
- [x] `nyrun startup` — generate + enable systemd unit (Linux)
- [x] `nyrun unstartup` — remove systemd unit
- [x] `nyrun kill` — stop daemon + all managed processes

## Process Manager
- [x] Spawn binary processes with custom env vars
- [x] `--env-file` dotenv loading
- [x] `--args` pass arguments to binary
- [x] Process lifecycle: start, stop, restart, delete
- [x] Graceful reload (zero-downtime) — currently same as restart
- [x] Auto-restart on crash (health check loop every 2s)
- [x] `nyrun ls` — list all processes with status
- [x] `nyrun del <name>` — stop and remove
- [x] `nyrun restart <name>`
- [x] `nyrun reload <name>`
- [x] `nyrun update <name>` — update config without removing
- [x] State persistence — auto-recover processes on nyrun startup

## Reverse Proxy (Pingora)
- [x] Basic proxy: `--p PUBLIC_PORT:APP_PORT`
- [x] Host-based routing: `--p HOST:PUBLIC_PORT:APP_PORT`
- [x] Single port listen: `--p PORT`
- [x] Multiple services sharing same public port via different hosts

## SPA Static File Serving
- [x] `--spa` flag — serve directory as SPA
- [x] Fallback to index.html for non-file routes
- [x] Content-type detection by file extension
- [x] Path traversal protection

## TLS/SSL
- [x] `--ssl CERT_PATH KEY_PATH` — manual certs
- [x] SNI-based dynamic certificate selection (DynamicCertStore + TlsAccept)
- [x] Multiple certs per listener (via shared DynamicCertStore)
- [x] Default cert fallback

## Auto SSL (ACME)
- [x] `--acme EMAIL` — Let's Encrypt integration
- [x] HTTP-01 challenge handler (`/.well-known/acme-challenge/`)
- [x] Cert storage (`/var/run/nyrun/certs/`)
- [x] Auto-renewal before expiry (12h check interval)
- [x] Host derived from `--p HOST:PORT:APP_PORT`

## In-Memory Caching (moka)
- [x] Tier 1: moka in-memory cache (10,000 entries, 60s TTL)
- [x] Cache key: `{host}{uri}{query}`
- [x] Bypass non-GET requests
- [x] Async cache save (spawned tokio task)
- [x] 5MB max body size per entry (OOM protection)
- [x] `X-Cache: HIT` header on cache hits

## OCI Support
- [x] Pull images from OCI registries (ghcr.io, etc.)
- [x] Extract layers to `/var/run/nyrun/oci/<name>/`
- [x] Execute extracted binaries natively
- [x] Isolated by default (eBPF sandbox to own folder on Linux)
- [x] `--allow PATHS` — whitelist additional directories
- [x] `--allow all` — disable isolation

## eBPF Sandboxing (Linux only)
- [x] `--deny net` — block socket syscalls per process (seccomp-BPF)
- [x] `--deny io` — restrict filesystem access outside working dir (Landlock)
- [x] `--allow PATHS` — whitelist comma-separated directories
- [x] Per-process enforcement, no container overhead

## Logging
- [x] stdout/stderr capture to `/var/run/nyrun/logs/`
- [x] `nyrun logs <name>` — tail logs
- [x] `nyrun logs <name> --lines N` — last N lines
- [x] Log rotation (10MB max, 5 rotated files, checked every ~60s)

## Observability (Prometheus)
- [x] `/metrics` endpoint on dedicated port (9090)
- [x] Process metrics: restart count per process
- [x] Proxy metrics: request count, latency histograms, status codes, active connections
- [x] Cache metrics: hit/miss counters
- [x] Process metrics: CPU/memory per process (sysinfo, collected every 2s)
- [ ] System metrics: total managed processes, OCI pull stats

## Backup/Restore
- [x] `nyrun backup -o <name>` — zip `/var/run/nyrun/`
- [x] `nyrun restore <file.zip>` — extract zip over `/var/run/nyrun/`

## Cloud Agent (nyrun side only — cloud UI is a separate private project)
- [ ] `nyrun link <api-key>` — connect to cloud
- [ ] `nyrun unlink` — disconnect from cloud
- [ ] Agent push: metrics, logs, status via WebSocket/gRPC
- [ ] Persistent outbound connection (no inbound ports needed)
- [ ] Heartbeat + reconnect with exponential backoff
- [ ] Receive and execute cloud → agent commands (restart, reload, del, update)
