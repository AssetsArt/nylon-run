use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "nyrun", about = "Process manager & reverse proxy", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run and manage a process (with optional reverse proxy via --p)
    Run {
        /// Path to binary/directory or OCI image reference
        path: String,
        /// Process name (defaults to binary filename)
        #[arg(long)]
        name: Option<String>,
        /// Port mapping: PORT, PUBLIC:APP, or HOST:PUBLIC:APP (enables reverse proxy)
        #[arg(long = "p")]
        port: Option<String>,
        /// Arguments to pass to the binary (quoted string)
        #[arg(long)]
        args: Option<String>,
        /// Path to .env file
        #[arg(long)]
        env_file: Option<String>,
        /// Serve directory as SPA (fallback to index.html)
        #[arg(long)]
        spa: bool,
        /// Manual TLS: cert and key paths
        #[arg(long, num_args = 2, value_names = ["CERT", "KEY"])]
        ssl: Option<Vec<String>>,
        /// Auto SSL via Let's Encrypt ACME (provide email)
        #[arg(long)]
        acme: Option<String>,
        /// Deny capabilities: net, io (comma-separated, Linux eBPF)
        #[arg(long)]
        deny: Option<String>,
        /// Allow paths when using --deny io (comma-separated)
        #[arg(long)]
        allow: Option<String>,
    },
    /// Start processes from a config file (YAML or JSON)
    Start {
        /// Path to config file (YAML default, JSON if .json extension)
        file: String,
        /// Start only this app by name
        #[arg(long)]
        only: Option<String>,
    },
    /// List all managed processes
    Ls,
    /// Stop and remove a process
    Del {
        /// Process name
        name: String,
    },
    /// Restart a process
    Restart {
        /// Process name
        name: String,
    },
    /// Graceful reload (zero-downtime)
    Reload {
        /// Process name
        name: String,
    },
    /// Update process config without removing
    Update {
        /// Process name
        name: String,
        /// New port mapping
        #[arg(long = "p")]
        port: Option<String>,
        /// New SSL cert and key
        #[arg(long, num_args = 2, value_names = ["CERT", "KEY"])]
        ssl: Option<Vec<String>>,
        /// New ACME email
        #[arg(long)]
        acme: Option<String>,
        /// New env file
        #[arg(long)]
        env_file: Option<String>,
        /// New args
        #[arg(long)]
        args: Option<String>,
        /// New OCI image reference (re-pull and update)
        #[arg(long)]
        image: Option<String>,
    },
    /// View process logs
    Logs {
        /// Process name
        name: String,
        /// Number of lines to show
        #[arg(long, default_value = "50")]
        lines: usize,
    },
    /// Export running processes as a config file (for use with `nyrun start`)
    Export {
        /// Output file path (default: print to stdout)
        #[arg(short, long)]
        o: Option<String>,
    },
    /// Set a global config value (e.g. cache-ttl)
    Set {
        /// Config key (e.g. cache-ttl)
        key: String,
        /// Config value
        value: String,
    },
    /// Save current process list for restore on reboot
    Save,
    /// Generate systemd unit + enable auto-start on boot
    Startup,
    /// Remove systemd unit
    Unstartup,
    /// Stop daemon and all managed processes
    Kill,
    /// Zip /var/run/nyrun/ as backup
    Backup {
        /// Output filename
        #[arg(short, long)]
        o: String,
    },
    /// Restore from backup zip
    Restore {
        /// Backup zip file path
        file: String,
    },
    /// Connect this instance to cloud UI
    Link {
        /// API key
        api_key: String,
    },
    /// Disconnect from cloud UI
    Unlink,
    /// Enable Prometheus metrics server
    #[command(name = "metrics")]
    Metrics {
        #[command(subcommand)]
        action: MetricsAction,
    },
    /// (internal) Run as daemon — not user-facing
    #[command(hide = true)]
    Daemon,
}

#[derive(Subcommand)]
pub enum MetricsAction {
    /// Start metrics server on specified port (default: 9090)
    Enable {
        /// Port to listen on
        #[arg(long, default_value = "9090")]
        port: u16,
    },
    /// Stop metrics server
    Disable,
}
