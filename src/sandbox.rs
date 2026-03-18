/// eBPF-based process sandboxing.
///
/// - `--deny net`: blocks network syscalls via seccomp-BPF
/// - `--deny io`: restricts filesystem access via Landlock LSM
/// - `--allow PATHS`: whitelists directories when io is denied
///
/// Linux only. On other platforms, returns an error if deny is non-empty.
#[cfg(target_os = "linux")]
mod linux {
    use landlock::{
        ABI, Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr,
        RulesetStatus,
    };
    use nix::libc;
    use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule};
    use std::collections::BTreeMap;
    use tracing::{info, warn};

    const LANDLOCK_ABI: ABI = ABI::V3;

    /// Apply network denial via seccomp-BPF.
    /// Must be called in the child process before exec.
    pub fn apply_deny_net() -> Result<(), String> {
        // Block socket-related syscalls
        let blocked_syscalls: Vec<i64> = vec![
            libc::SYS_socket,
            libc::SYS_connect,
            libc::SYS_bind,
            libc::SYS_listen,
            libc::SYS_accept,
            libc::SYS_accept4,
            libc::SYS_sendto,
            libc::SYS_sendmsg,
            libc::SYS_sendmmsg,
        ];

        let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
        for syscall in blocked_syscalls {
            rules.insert(
                syscall,
                vec![SeccompRule::new(vec![]).map_err(|e| format!("seccomp rule error: {e}"))?],
            );
        }

        let filter = SeccompFilter::new(
            rules,
            SeccompAction::Allow, // default: allow everything
            SeccompAction::Errno(libc::EPERM as u32), // matched syscalls: EPERM
            std::env::consts::ARCH
                .try_into()
                .map_err(|e| format!("unsupported arch: {e}"))?,
        )
        .map_err(|e| format!("seccomp filter error: {e}"))?;

        let bpf: BpfProgram = filter
            .try_into()
            .map_err(|e| format!("seccomp compile error: {e}"))?;

        seccompiler::apply_filter(&bpf).map_err(|e| format!("seccomp apply error: {e}"))?;

        info!("seccomp network deny applied");
        Ok(())
    }

    /// Apply filesystem restriction via Landlock.
    /// Allows access only to `allowed_paths` (and basic read for system libs).
    pub fn apply_deny_io(allowed_paths: &[String], working_dir: &str) -> Result<(), String> {
        let abi = LANDLOCK_ABI;

        let mut ruleset = Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .map_err(|e| format!("landlock ruleset error: {e}"))?
            .create()
            .map_err(|e| format!("landlock create error: {e}"))?;

        let read_access = AccessFs::ReadFile | AccessFs::ReadDir | AccessFs::Execute;
        let full_access = AccessFs::from_all(abi);

        // Always allow read access to system directories for dynamic linking
        let system_read_dirs = [
            "/lib",
            "/lib64",
            "/usr/lib",
            "/usr/lib64",
            "/usr/local/lib",
            "/etc/ld.so.cache",
            "/etc/ld.so.conf",
            "/etc/ld.so.conf.d",
            "/proc/self",
            "/dev/null",
            "/dev/urandom",
            "/dev/zero",
        ];

        for dir in system_read_dirs {
            if let Ok(fd) = PathFd::new(dir) {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(fd, read_access))
                    .map_err(|e| format!("landlock add rule error: {e}"))?;
            }
        }

        // Allow full access to working directory
        if let Ok(fd) = PathFd::new(working_dir) {
            ruleset = ruleset
                .add_rule(PathBeneath::new(fd, full_access))
                .map_err(|e| format!("landlock add rule error: {e}"))?;
        }

        // Allow full access to explicitly allowed paths
        for path in allowed_paths {
            if let Ok(fd) = PathFd::new(path) {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(fd, full_access))
                    .map_err(|e| format!("landlock add rule error: {e}"))?;
            } else {
                warn!(path, "allowed path not found, skipping");
            }
        }

        // Always allow access to nyrun logs directory
        if let Ok(fd) = PathFd::new("/var/run/nyrun/logs") {
            ruleset = ruleset
                .add_rule(PathBeneath::new(fd, full_access))
                .map_err(|e| format!("landlock add rule error: {e}"))?;
        }

        // Allow /tmp for temporary files
        if let Ok(fd) = PathFd::new("/tmp") {
            ruleset = ruleset
                .add_rule(PathBeneath::new(fd, full_access))
                .map_err(|e| format!("landlock add rule error: {e}"))?;
        }

        let status = ruleset
            .restrict_self()
            .map_err(|e| format!("landlock restrict error: {e}"))?;

        match status.ruleset {
            RulesetStatus::FullyEnforced => info!("landlock filesystem deny fully enforced"),
            RulesetStatus::PartiallyEnforced => {
                warn!("landlock filesystem deny partially enforced")
            }
            RulesetStatus::NotEnforced => warn!("landlock not supported on this kernel"),
        }

        Ok(())
    }
}

/// Apply sandbox restrictions to the current process.
/// Called from the child process (via pre_exec or command setup).
///
/// `deny` - list of capabilities to deny: "net", "io"
/// `allow` - list of paths to allow when io is denied
/// `working_dir` - the process working directory (always allowed for io)
#[cfg(target_os = "linux")]
pub fn apply_sandbox(deny: &[String], allow: &[String], working_dir: &str) -> Result<(), String> {
    if deny.is_empty() {
        return Ok(());
    }

    for cap in deny {
        match cap.as_str() {
            "net" => linux::apply_deny_net()?,
            "io" => linux::apply_deny_io(allow, working_dir)?,
            other => return Err(format!("unknown deny capability: '{other}'")),
        }
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn apply_sandbox(deny: &[String], _allow: &[String], _working_dir: &str) -> Result<(), String> {
    if deny.is_empty() {
        return Ok(());
    }
    Err("sandboxing (--deny) is only supported on Linux".to_string())
}
