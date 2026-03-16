# nylon-run Implementation Checklist

## Core Infrastructure
- [ ] Project setup (Cargo.toml dependencies, module structure)
- [ ] CLI parsing with clap (`bin`, `run`, `ls`, `del`, `restart`, `reload`, `update`, `logs`, `save`, `startup`, `unstartup`, `kill`, `backup`, `restore`, `link`, `unlink`)
- [ ] SlateDB persistent state (`/tmp/nyrun/state/`)
- [ ] Working directory setup (`/tmp/nyrun/`)

## Daemon
- [ ] Auto-start daemon on first command, PID file at `/tmp/nyrun/nyrun.pid`
- [ ] CLI ↔ daemon communication (Unix socket)
- [ ] `nyrun save` — snapshot current process list for restore on reboot
- [ ] `nyrun startup` — generate + enable systemd unit (Linux)
- [ ] `nyrun unstartup` — remove systemd unit
- [ ] `nyrun kill` — stop daemon + all managed processes

## Process Manager
- [ ] Spawn binary processes with custom env vars
- [ ] `--env-file` dotenv loading
- [ ] Process lifecycle: start, stop, restart, delete
- [ ] Graceful reload (zero-downtime)
- [ ] Auto-restart on crash
- [ ] `nyrun ls` — list all processes with status
- [ ] `nyrun del <name>` — stop and remove
- [ ] `nyrun restart <name>`
- [ ] `nyrun reload <name>`
- [ ] `nyrun update <name>` — update config without removing
- [ ] State persistence — auto-recover processes on nyrun startup

## Reverse Proxy (Pingora)
- [ ] Basic proxy: `--p PUBLIC_PORT:APP_PORT`
- [ ] Host-based routing: `--p HOST:PUBLIC_PORT:APP_PORT`
- [ ] Single port listen: `--p PORT`
- [ ] Multiple services sharing same public port via different hosts

## SPA Static File Serving
- [ ] `--spa` flag — serve directory as SPA
- [ ] Fallback to index.html for non-file routes

## TLS/SSL
- [ ] `--ssl CERT_PATH KEY_PATH` — manual certs
- [ ] SNI-based dynamic certificate selection (DynamicCertificate + TlsAccept)
- [ ] Multiple certs per listener
- [ ] Default cert fallback

## Auto SSL (ACME)
- [ ] `--acme EMAIL` — Let's Encrypt integration
- [ ] HTTP-01 challenge handler (`/.well-known/acme-challenge/`)
- [ ] Cert storage (`/tmp/nyrun/certs/`)
- [ ] Auto-renewal before expiry
- [ ] Host derived from `--p HOST:PORT:APP_PORT`

## Two-Tier Caching
- [ ] Tier 1: moka in-memory cache (configurable capacity + TTL)
- [ ] Tier 2: Redis distributed cache (HSET storage, JSON-serialized headers)
- [ ] Cache key: `{host}{uri}{query}:{encoding}`
- [ ] Encoding-aware (gzip/br/zstd/deflate variants)
- [ ] Bypass rules (non-GET, path prefixes, file extensions)
- [ ] Async cache save (spawned tokio task)
- [ ] Smart encoding selection (hit frequency tracking)

## OCI Support
- [ ] Pull images from OCI registries (ghcr.io, etc.)
- [ ] Extract layers to `/tmp/nyrun/oci/<name>/`
- [ ] Execute extracted binaries natively
- [ ] Isolated by default (eBPF sandbox to own folder on Linux)
- [ ] `--allow PATHS` — whitelist additional directories
- [ ] `--allow all` — disable isolation

## eBPF Sandboxing (Linux only)
- [ ] `--deny net` — block socket syscalls per process
- [ ] `--deny io` — restrict filesystem access outside working dir
- [ ] `--allow PATHS` — whitelist comma-separated directories
- [ ] Per-process enforcement, no container overhead

## Logging
- [ ] stdout/stderr capture to `/tmp/nyrun/logs/`
- [ ] `nyrun logs <name>` — tail logs
- [ ] `nyrun logs <name> --lines N` — last N lines
- [ ] Log rotation

## Observability (Prometheus)
- [ ] `/metrics` endpoint on dedicated port
- [ ] Process metrics: uptime, restart count, CPU/memory per process
- [ ] Proxy metrics: request count, latency histograms, status codes, active connections
- [ ] Cache metrics: hit/miss ratio (T1/T2), cache size, eviction count
- [ ] System metrics: total managed processes, OCI pull stats

## Backup/Restore
- [ ] `nyrun backup -o <name>` — zip `/tmp/nyrun/`
- [ ] `nyrun restore <file.zip>` — extract zip over `/tmp/nyrun/`

## Cloud Agent (nyrun side only — cloud UI is a separate private project)
- [ ] `nyrun link <api-key>` — connect to cloud
- [ ] `nyrun unlink` — disconnect from cloud
- [ ] Agent push: metrics, logs, status via WebSocket/gRPC
- [ ] Persistent outbound connection (no inbound ports needed)
- [ ] Heartbeat + reconnect with exponential backoff
- [ ] Receive and execute cloud → agent commands (restart, reload, del, update)
