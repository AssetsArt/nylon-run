<div align="center">

# Nylon Run

**Run Anything. Proxy Everything.**

[![Rust](https://img.shields.io/badge/Rust-000000?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![CI](https://github.com/AssetsArt/nylon-run/actions/workflows/ci.yml/badge.svg)](https://github.com/AssetsArt/nylon-run/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-MIT-green)](#)

*A language-agnostic process manager and reverse proxy built in Rust with Pingora.*

[Features](#features) • [Install](#installation) • [Usage](#usage) • [Config](#config-file)

</div>

## Features

- **Process Manager** — spawn, monitor, auto-restart on crash, log capture
- **Reverse Proxy** — powered by [Pingora](https://github.com/cloudflare/pingora) (Cloudflare's proxy framework)
- **Host-Based Routing** — multiple services sharing the same port via different hostnames
- **Auto SSL** — automatic Let's Encrypt certificates via ACME (HTTP-01)
- **SPA Static Serving** — serve any directory as a single-page application
- **In-Memory Cache** — built-in response caching (moka, 10k entries, 60s TTL)
- **Native OCI Execution** — pull any Docker/OCI image, extract it, and run the binary directly as a native host process — bypassing container runtimes entirely
- **eBPF Sandboxing** — restrict network/filesystem access per process at the kernel level (Linux). OCI processes are sandboxed to their own directory by default
- **Prometheus Metrics** — opt-in `/metrics` endpoint for Grafana dashboards (`nyrun metrics enable`)
- **K8s-Style Config** — multi-document YAML manifests with ConfigMap and volume mounts
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

### Process Management

```bash
# Process only (no proxy)
nyrun run ./my-app
nyrun run ./my-app --args "--port 8000 --verbose"
nyrun run ./my-app --env-file .env

# With reverse proxy (add --p)
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

### OCI Images — Containerless Execution

Stop building from source. Nylon Run extracts and executes binaries from any Docker/OCI image natively on your host.

```bash
# Short name — defaults to Docker Hub
nyrun run nginx:latest --p 80:80
nyrun run traefik:v3.6 --p 80:80
nyrun run redis:7

# Full reference
nyrun run ghcr.io/org/app:latest --p 8081:8081

# OCI with full filesystem access (bypass default sandbox)
nyrun run ghcr.io/org/app:latest --allow all --p 8081:8081
```

OCI images are extracted to `/var/run/nyrun/oci/<name>/` and executed as native processes. Each OCI process is automatically sandboxed to its own directory via eBPF — no container runtime, no namespaces, zero overhead.

### eBPF Sandboxing (Linux)

Kernel-level process isolation without containers:

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

### Metrics & Settings

```bash
nyrun metrics enable              # start Prometheus metrics on :9090
nyrun metrics enable --port 9100  # custom port
nyrun metrics disable             # stop metrics server

nyrun set default-registry docker.io     # default OCI registry
nyrun set default-registry ghcr.io       # switch to GHCR
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

## Config File

Kubernetes-style manifests — multi-document YAML with `---` separators:

```bash
nyrun start ecosystem.yaml
nyrun start ecosystem.yaml --only api
nyrun export -o ecosystem.yaml        # export running processes
```

```yaml
kind: ConfigMap
metadata:
  name: app-config
data:
  config.yaml: |
    database:
      host: localhost
      port: 5432
  nginx.conf: |
    server {
      listen 80;
      location / { proxy_pass http://localhost:8000; }
    }
---
kind: Process
metadata:
  name: api
spec:
  path: ./api-server
  port: "api.example.com:443:8000"
  args: "--verbose"
  env_file: .env
  env:
    NODE_ENV: production
  acme: user@example.com
  volumes:
    - configmap:app-config:/etc/app
---
kind: Process
metadata:
  name: nginx
spec:
  path: nginx:latest
  port: "80:80"
  volumes:
    - configmap:app-config:/etc/nginx
    - ./html:/usr/share/nginx/html
---
kind: Process
metadata:
  name: worker
spec:
  path: ./worker
  deny: net
```

### ConfigMap

Define configuration data inline and mount it into processes:

```yaml
kind: ConfigMap
metadata:
  name: my-config
data:
  app.yaml: |
    key: value
  settings.json: |
    {"debug": true}
```

ConfigMap files are written to `/var/run/nyrun/configmaps/<name>/` and can be mounted via volumes.

### Volume Mounts

Mount host files, directories, or ConfigMaps into the process working directory:

```yaml
spec:
  volumes:
    - ./local-file.conf:/etc/app/app.conf      # host file
    - ./templates:/app/templates                 # host directory
    - configmap:my-config:/etc/app               # ConfigMap
```

## Architecture

| Component | Technology |
|-----------|-----------|
| Proxy | [Pingora](https://github.com/cloudflare/pingora) |
| Cache | [moka](https://github.com/moka-rs/moka) |
| State | [SlateDB](https://github.com/slatedb/slatedb) |
| ACME | [instant-acme](https://github.com/InstantDomain/instant-acme) |
| Metrics | [prometheus-client](https://github.com/prometheus/client_rust) |
| Sandbox | eBPF (Landlock + seccomp) |
| Allocator | [mimalloc](https://github.com/purpleprotocol/mimalloc_rust) |

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
