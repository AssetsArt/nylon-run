<div align="center">

# Nylon Run

**Run Anything. Proxy Everything.**

[![Rust](https://img.shields.io/badge/Rust-000000?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![CI](https://github.com/AssetsArt/nylon-run/actions/workflows/ci.yml/badge.svg)](https://github.com/AssetsArt/nylon-run/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-MIT-green)](#)

*A language-agnostic process manager and reverse proxy — like PM2, but built in Rust with Pingora.*

[Why Nylon Run?](#why-nylon-run) • [Features](#features) • [Install](#installation) • [Usage](#usage) • [Ecosystem File](#ecosystem-file)

</div>

## Why Nylon Run?

Managing production services usually means juggling multiple tools — a process manager, a reverse proxy, TLS certificates, and monitoring dashboards. **Nylon Run combines all of that into a single binary** with zero config files. Everything is driven by CLI commands and persisted automatically.

- No YAML/JSON config to maintain
- No separate Nginx/Caddy setup
- No manual certificate management
- Works with **any** language or binary

## Features

- **Process Manager** — spawn, monitor, auto-restart on crash, log capture
- **Reverse Proxy** — powered by [Pingora](https://github.com/cloudflare/pingora) (Cloudflare's proxy framework)
- **Host-Based Routing** — multiple services sharing the same port via different hostnames
- **Auto SSL** — automatic Let's Encrypt certificates via ACME (HTTP-01)
- **SPA Static Serving** — serve any directory as a single-page application
- **In-Memory Cache** — built-in response caching (moka, 10k entries, 60s TTL)
- **OCI Support** — pull and run container images natively, no Docker required
- **eBPF Sandboxing** — restrict network/filesystem access per process (Linux)
- **Prometheus Metrics** — built-in `/metrics` endpoint for Grafana dashboards
- **Backup/Restore** — zip/unzip the entire runtime state
- **Cloud Agent** — optional remote management via WebSocket

## Installation

```bash
curl -fsSL https://raw.githubusercontent.com/AssetsArt/nylon-run/main/docs/public/install | bash
```

Or build from source:

```bash
cargo build --release
```

## Usage

### Process Management (no proxy)

```bash
nyrun bin ./my-app
nyrun bin ./my-app --args "--port 8000 --verbose"
nyrun bin ./my-app --env-file .env
```

### Process + Reverse Proxy

```bash
# Simple port mapping (public:internal)
nyrun run ./my-app --p 80:8000

# Host-based routing
nyrun run ./app1 --p example.com:80:8000
nyrun run ./app2 --p other.com:80:9000

# SPA static file serving
nyrun run ./dist --spa --p 8080

# HTTPS with auto Let's Encrypt
nyrun run ./my-app --p example.com:443:8000 --acme user@example.com

# HTTPS with manual certs
nyrun run ./my-app --p example.com:443:8000 --ssl cert.pem key.pem
```

### OCI Images

```bash
# Short name — defaults to Docker Hub (docker.io/library/)
nyrun run nginx:latest --p 80:80
nyrun run traefik:v3.6 --p 80:80

# Full reference
nyrun run ghcr.io/org/app:latest --p 8081:8081

# OCI with full filesystem access
nyrun run ghcr.io/org/app:latest --allow all --p 8081:8081
```

### eBPF Sandboxing (Linux)

```bash
# Deny network access
nyrun run ./my-app --deny net --p 80:8000

# Deny filesystem I/O except specific paths
nyrun run ./my-app --deny io --allow /tmp/data,/var/log --p 80:8000
```

### Management Commands

```bash
nyrun ls                    # list all processes
nyrun logs <name>           # tail logs
nyrun logs <name> --lines 100
nyrun restart <name>        # restart a process
nyrun reload <name>         # graceful zero-downtime reload
nyrun del <name>            # stop and remove
nyrun update <name> --p 443:8000 --acme user@example.com
```

### Global Settings

```bash
nyrun set default-registry docker.io     # default — Docker Hub
nyrun set default-registry ghcr.io       # switch to GitHub Container Registry
nyrun set cache-ttl 120                  # proxy cache TTL in seconds
```

### System Commands

```bash
nyrun save                  # save process list for restore on reboot
nyrun startup               # generate systemd unit for auto-start
nyrun unstartup             # remove systemd unit
nyrun kill                  # stop daemon + all processes
nyrun backup -o backup      # zip runtime state
nyrun restore backup.zip    # restore from backup
```

### Cloud

```bash
nyrun link <api-key>        # connect to cloud UI
nyrun unlink                # disconnect
```

## Ecosystem File

Start multiple apps from a single JSON config (like PM2):

```bash
nyrun start ecosystem.json
nyrun start ecosystem.json --only api
```

```json
{
  "apps": [
    {
      "name": "api",
      "path": "./api-server",
      "port": "api.example.com:443:8000",
      "args": "--verbose",
      "env_file": ".env",
      "env": { "NODE_ENV": "production" },
      "acme": "user@example.com"
    },
    {
      "name": "worker",
      "path": "./worker",
      "deny": "net"
    }
  ]
}
```

## Architecture

| Component | Technology |
|-----------|-----------|
| Proxy | [Pingora](https://github.com/cloudflare/pingora) |
| Cache | [moka](https://github.com/moka-rs/moka) |
| State | [SlateDB](https://github.com/slatedb/slatedb) |
| ACME | [instant-acme](https://github.com/InstantDomain/instant-acme) |
| Metrics | [prometheus-client](https://github.com/prometheus/client_rust) |
| Allocator | [mimalloc](https://github.com/purpleprotocol/mimalloc_rust) |

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
