use std::{
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use serde::Serialize;
use thiserror::Error;

const DEFAULT_TARGET_ROOT: &str = "/mnt";
const DEFAULT_REMOTE_WORKDIR: &str = "/tmp/nixos-kexec";
const DEFAULT_DISK_NIX: &str = "github:hermetic-foundation/disk-nix#disk-nix";
const DEFAULT_DISK_NIX_FLAKE: &str = "github:hermetic-foundation/disk-nix";
const DEFAULT_NIXOS_KEXEC_FLAKE: &str = "github:hermetic-foundation/nixos-kexec";
const DEFAULT_NIXPKGS_FLAKE: &str = "github:NixOS/nixpkgs/nixos-unstable";
const GENERATED_HOST_KEY_VAR: &str = "$NIXOS_KEXEC_HOST_KEY";
const GENERATED_HOST_KEY_PUBLIC_VAR: &str = "$NIXOS_KEXEC_HOST_KEY_PUBLIC";
const RAW_SCRIPT_SENTINEL: &str = "__nixos_kexec_raw_script";

#[derive(Debug, Parser)]
#[command(
    name = "nixos-kexec",
    version,
    about = "Deploy NixOS through kexec with disk-nix storage management"
)]
pub struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// Plan an SSH kexec deployment without running commands.
    Plan(SshCommand),
    /// Render a reviewable local orchestration script.
    Script(SshCommand),
    /// Execute the generated local orchestration script.
    Run(SshRunCommand),
    /// Build or render commands for the upstream kexec installer tree.
    Installer(InstallerCommand),
    /// Generate shell completions.
    Completions { shell: Shell },
}

#[derive(Debug, Args)]
pub struct SshRunCommand {
    #[command(flatten)]
    ssh: SshCommand,
    /// Actually run the generated local orchestration script.
    #[arg(long)]
    execute: bool,
}

#[derive(Debug, Subcommand)]
pub enum InstallerAction {
    /// Render the Nix build command for the installer tree.
    Plan(InstallerBuildCommand),
    /// Build the installer tree. Refuses to run without --execute.
    Build(InstallerRunCommand),
}

#[derive(Debug, Args)]
pub struct InstallerCommand {
    #[command(subcommand)]
    action: InstallerAction,
}

#[derive(Debug, Args, Clone)]
pub struct InstallerRunCommand {
    #[command(flatten)]
    build: InstallerBuildCommand,
    /// Actually run the generated Nix build command.
    #[arg(long)]
    execute: bool,
}

#[derive(Debug, Args, Clone)]
pub struct InstallerBuildCommand {
    /// SSH authorized_keys public key file. Can be passed more than once.
    #[arg(long = "authorized-key-file", value_name = "PATH")]
    authorized_key_files: Vec<PathBuf>,
    /// Literal SSH public key. Can be passed more than once.
    #[arg(long = "authorized-key", value_name = "KEY")]
    authorized_keys: Vec<String>,
    /// JSON file containing NetworkManager profiles for the installer tree.
    #[arg(long = "network-manager-profiles-json", value_name = "PATH")]
    network_manager_profiles_json: Option<PathBuf>,
    /// Target system for the installer tree.
    #[arg(long, default_value = "x86_64-linux")]
    system: String,
    /// Flake containing examples/kexec-installer.nix.
    #[arg(long, default_value = DEFAULT_NIXOS_KEXEC_FLAKE)]
    nixos_kexec_flake: String,
    /// disk-nix flake used by the installer tree.
    #[arg(long, default_value = DEFAULT_DISK_NIX_FLAKE)]
    disk_nix_flake: String,
    /// nixpkgs flake used by the installer tree.
    #[arg(long, default_value = DEFAULT_NIXPKGS_FLAKE)]
    nixpkgs_flake: String,
    /// Create or update this result symlink.
    #[arg(long, value_name = "PATH")]
    out_link: Option<PathBuf>,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args, Clone)]
pub struct SshCommand {
    /// SSH target such as root@192.0.2.10.
    #[arg(value_name = "SSH_TARGET")]
    target: String,
    /// NixOS flake installable such as github:you/flake#host.
    #[arg(long)]
    flake: String,
    /// Local flake directory to upload into the installer before nixos-install.
    #[arg(long, value_name = "PATH")]
    flake_source: Option<PathBuf>,
    /// SSH known_hosts file to install inside the kexec installer before flake evaluation.
    ///
    /// Pass this for private git+ssh flake inputs so Nix/Git inside the
    /// installer can verify the Git server without an interactive prompt. Can
    /// be passed more than once.
    #[arg(long = "installer-known-hosts-file", value_name = "PATH")]
    installer_known_hosts_files: Vec<PathBuf>,
    /// Prebuilt NixOS system closure to copy into the target and install.
    #[arg(long, value_name = "STORE_PATH")]
    system: Option<PathBuf>,
    /// disk-nix install spec to upload and apply.
    #[arg(long, value_name = "PATH")]
    disk_spec: PathBuf,
    /// Kernel file to upload and load with kexec.
    #[arg(long, value_name = "PATH")]
    kexec_kernel: PathBuf,
    /// Initrd file to upload and load with kexec.
    #[arg(long, value_name = "PATH")]
    kexec_initrd: PathBuf,
    /// Kernel command line for the kexec installer environment.
    ///
    /// If omitted, nixos-kexec reads the command line from a sibling
    /// `kexec-boot` script when the kernel comes from a NixOS kexec tree.
    #[arg(long)]
    kexec_append: Option<String>,
    /// disk-nix flake app used inside the installer environment.
    #[arg(long, default_value = DEFAULT_DISK_NIX)]
    disk_nix: String,
    /// Preinstalled disk-nix-compatible command to run instead of `nix run`.
    #[arg(long, value_name = "COMMAND")]
    disk_nix_command: Option<String>,
    /// Local private SSH host key to install into the target before nixos-install.
    ///
    /// The key is copied over SSH at deploy time and installed as
    /// /etc/ssh/ssh_host_ed25519_key under the target root. Keep this file
    /// outside the flake source and outside the Nix store.
    #[arg(long, value_name = "PATH")]
    host_key: Option<PathBuf>,
    /// Local public SSH host key to install next to --host-key.
    ///
    /// If omitted and <host-key>.pub exists, that file is used automatically.
    #[arg(long, value_name = "PATH")]
    host_key_public: Option<PathBuf>,
    /// Generate a temporary Ed25519 SSH host key and install it into the target.
    ///
    /// The local private key is created in a private temp directory and removed
    /// by the generated orchestration script on success or failure.
    #[arg(long)]
    generate_host_key: bool,
    /// Shell command to run after host identity material is available locally.
    ///
    /// The hook receives NIXOS_KEXEC_IDENTITY_HOST,
    /// NIXOS_KEXEC_SSH_PUBLIC_KEY_FILE, NIXOS_KEXEC_SSH_PUBLIC_KEY, and
    /// NIXOS_KEXEC_AGE_RECIPIENT. It should be idempotent.
    #[arg(long, value_name = "COMMAND")]
    identity_hook: Option<String>,
    /// Shell command to run if deployment fails after the identity hook starts.
    ///
    /// The rollback hook receives the same identity environment as
    /// --identity-hook plus NIXOS_KEXEC_IDENTITY_HOOK_EVENT=rollback.
    #[arg(long, value_name = "COMMAND")]
    identity_rollback_hook: Option<String>,
    /// Logical host name passed to --identity-hook.
    ///
    /// Defaults to the fragment in --flake when present.
    #[arg(long, value_name = "HOST")]
    identity_host: Option<String>,
    /// Mount target passed to disk-nix install nixos.
    #[arg(long, default_value = DEFAULT_TARGET_ROOT)]
    target_root: String,
    /// Remote temporary work directory.
    #[arg(long, default_value = DEFAULT_REMOTE_WORKDIR)]
    remote_workdir: String,
    /// Extra option passed to ssh and scp.
    #[arg(long = "ssh-option", value_name = "OPTION")]
    ssh_options: Vec<String>,
    /// Allocate a TTY for mutating remote SSH commands that may prompt.
    #[arg(long)]
    ssh_tty: bool,
    /// Write the rendered script to this path.
    #[arg(long, value_name = "PATH")]
    script_out: Option<PathBuf>,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    json: bool,
    /// Stop after the disk-nix install handoff instead of rebooting.
    #[arg(long)]
    no_final_reboot: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Preflight,
    StageKexec,
    Kexec,
    AwaitInstaller,
    StageInstall,
    DiskNixApply,
    CopySystem,
    StageHostIdentity,
    NixosInstall,
    Reboot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentPlan {
    pub target: String,
    pub flake: String,
    pub install_flake: String,
    pub install_strategy: InstallStrategy,
    pub install_strategy_summary: String,
    pub flake_source: Option<String>,
    pub installer_known_hosts_files: Vec<String>,
    pub system: Option<String>,
    pub disk_spec: String,
    pub disk_nix: String,
    pub disk_nix_command: Option<String>,
    pub host_key: Option<String>,
    pub host_key_public: Option<String>,
    pub generate_host_key: bool,
    pub identity_hook: Option<String>,
    pub identity_rollback_hook: Option<String>,
    pub identity_host: Option<String>,
    pub ssh_tty: bool,
    pub target_root: String,
    pub remote_workdir: String,
    pub commands: Vec<PlanCommand>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallStrategy {
    InstallerBuildsFlake,
    StageLocalFlake,
    CopyPrebuiltSystem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallerBuildPlan {
    pub system: String,
    pub nixos_kexec_flake: String,
    pub disk_nix_flake: String,
    pub nixpkgs_flake: String,
    pub authorized_key_count: usize,
    pub network_manager_profiles_json: Option<String>,
    pub out_link: Option<String>,
    pub argv: Vec<String>,
    pub expression: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanCommand {
    pub phase: Phase,
    pub argv: Vec<String>,
    pub description: String,
    pub mutates: bool,
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub fn run(cli: Cli, output: &mut impl Write) -> Result<(), AppError> {
    match cli.command {
        CommandKind::Plan(command) => {
            let plan = build_plan(&command)?;
            if command.json {
                writeln!(output, "{}", serde_json::to_string_pretty(&plan)?)?;
            } else {
                print_plan(output, &plan)?;
            }
        }
        CommandKind::Script(command) => {
            let plan = build_plan(&command)?;
            let script = render_script(&plan);
            write_or_print_script(output, command.script_out.as_deref(), &script)?;
        }
        CommandKind::Run(command) => {
            if !command.execute {
                return Err(AppError::Message(
                    "run refuses to execute without --execute; use `nixos-kexec script` for review"
                        .to_string(),
                ));
            }
            let plan = build_plan(&command.ssh)?;
            let script = render_script(&plan);
            write_or_print_script(output, command.ssh.script_out.as_deref(), &script)?;
            let status = Command::new("bash").arg("-c").arg(script).status()?;
            if !status.success() {
                return Err(AppError::Message(format!(
                    "orchestration script failed with status {status}"
                )));
            }
        }
        CommandKind::Installer(command) => match command.action {
            InstallerAction::Plan(command) => {
                let plan = build_installer_plan(&command)?;
                if command.json {
                    writeln!(output, "{}", serde_json::to_string_pretty(&plan)?)?;
                } else {
                    print_installer_plan(output, &plan)?;
                }
            }
            InstallerAction::Build(command) => {
                if !command.execute {
                    return Err(AppError::Message(
                        "installer build refuses to run without --execute; use `nixos-kexec installer plan` for review"
                            .to_string(),
                    ));
                }
                let plan = build_installer_plan(&command.build)?;
                if command.build.json {
                    writeln!(output, "{}", serde_json::to_string_pretty(&plan)?)?;
                } else {
                    print_installer_plan(output, &plan)?;
                }
                let status = Command::new(&plan.argv[0])
                    .args(&plan.argv[1..])
                    .stdin(Stdio::null())
                    .status()?;
                if !status.success() {
                    return Err(AppError::Message(format!(
                        "installer build failed with status {status}"
                    )));
                }
            }
        },
        CommandKind::Completions { shell } => {
            let mut command = Cli::command();
            let name = command.get_name().to_string();
            generate(shell, &mut command, name, output);
        }
    }
    Ok(())
}

pub fn build_installer_plan(
    command: &InstallerBuildCommand,
) -> Result<InstallerBuildPlan, AppError> {
    let authorized_keys = read_authorized_keys(command)?;
    if authorized_keys.is_empty() {
        return Err(AppError::Message(
            "installer tree requires at least one --authorized-key-file or --authorized-key"
                .to_string(),
        ));
    }
    if command.system.trim().is_empty() {
        return Err(AppError::Message(
            "installer system cannot be empty".to_string(),
        ));
    }
    if let Some(path) = &command.network_manager_profiles_json {
        require_file("NetworkManager profiles JSON", path)?;
    }

    let expression = installer_expression(command, &authorized_keys);
    let mut argv = vec![
        "nix".to_string(),
        "build".to_string(),
        "--impure".to_string(),
        "--expr".to_string(),
        expression.clone(),
    ];
    if let Some(out_link) = &command.out_link {
        argv.push("--out-link".to_string());
        argv.push(out_link.display().to_string());
    }

    Ok(InstallerBuildPlan {
        system: command.system.clone(),
        nixos_kexec_flake: command.nixos_kexec_flake.clone(),
        disk_nix_flake: command.disk_nix_flake.clone(),
        nixpkgs_flake: command.nixpkgs_flake.clone(),
        authorized_key_count: authorized_keys.len(),
        network_manager_profiles_json: command
            .network_manager_profiles_json
            .as_ref()
            .map(|path| path.display().to_string()),
        out_link: command
            .out_link
            .as_ref()
            .map(|path| path.display().to_string()),
        argv,
        expression,
        warnings: vec![
            "installer builds are local; pass the current SSH target only to plan/script/run"
                .to_string(),
            "review authorized keys and any NetworkManager profile embedded in the installer tree"
                .to_string(),
        ],
    })
}

pub fn build_plan(command: &SshCommand) -> Result<DeploymentPlan, AppError> {
    require_file("disk spec", &command.disk_spec)?;
    require_file("kexec kernel", &command.kexec_kernel)?;
    require_file("kexec initrd", &command.kexec_initrd)?;
    if !command.target.contains('@') {
        return Err(AppError::Message(
            "SSH target should include a user, for example root@192.0.2.10".to_string(),
        ));
    }
    if !command.target_root.starts_with('/') || !command.remote_workdir.starts_with('/') {
        return Err(AppError::Message(
            "target root and remote workdir must be absolute paths".to_string(),
        ));
    }
    if let Some(flake_source) = &command.flake_source {
        if !flake_source.is_dir() {
            return Err(AppError::Message(format!(
                "flake source is not a directory: {}",
                flake_source.display()
            )));
        }
    }
    for path in &command.installer_known_hosts_files {
        require_file("installer known_hosts file", path)?;
    }
    if let Some(system) = &command.system {
        require_existing_path("system closure", system)?;
    }
    if command.host_key.is_some() && command.generate_host_key {
        return Err(AppError::Message(
            "--host-key and --generate-host-key are mutually exclusive".to_string(),
        ));
    }
    if command.host_key.is_none() && command.host_key_public.is_some() {
        return Err(AppError::Message(
            "--host-key-public requires --host-key".to_string(),
        ));
    }
    if let Some(host_key) = &command.host_key {
        require_file("host key", host_key)?;
    }
    let host_key_public = effective_host_key_public(command)?;
    if command.system.is_some() && command.flake_source.is_some() {
        return Err(AppError::Message(
            "--system and --flake-source select different install strategies; use --system for a prebuilt closure or --flake-source for installer-side flake install"
                .to_string(),
        ));
    }
    if command.system.is_some() && command.identity_hook.is_some() {
        return Err(AppError::Message(
            "--identity-hook cannot update secrets inside an already built --system closure; run the hook before building --system or use --flake-source"
                .to_string(),
        ));
    }
    if command.identity_hook.is_some() && !has_host_identity(command) {
        return Err(AppError::Message(
            "--identity-hook requires --host-key or --generate-host-key".to_string(),
        ));
    }
    if command.identity_rollback_hook.is_some() && command.identity_hook.is_none() {
        return Err(AppError::Message(
            "--identity-rollback-hook requires --identity-hook".to_string(),
        ));
    }
    let identity_host = effective_identity_host(command)?;

    let remote_spec = format!("{}/disk-nix-install.json", command.remote_workdir);
    let remote_kernel = format!("{}/kexec/kernel", command.remote_workdir);
    let remote_initrd = format!("{}/kexec/initrd", command.remote_workdir);
    let remote_flake_source = format!("{}/flake-source", command.remote_workdir);
    let kexec_append = effective_kexec_append(command)?;
    let install_flake = install_flake(command, &remote_flake_source)?;
    let install_strategy = install_strategy(command);
    let install_strategy_summary = install_strategy_summary(install_strategy);
    let mut commands = Vec::new();

    if command.generate_host_key {
        commands.push(generate_host_key_command(command, identity_host.as_deref()));
    } else if command.identity_hook.is_some() {
        commands.push(identity_state_command());
    }
    if let Some(rollback_hook) = &command.identity_rollback_hook {
        commands.push(identity_rollback_trap_command(
            command,
            rollback_hook,
            identity_host.as_deref(),
            host_key_public.as_deref(),
        ));
    }
    if let Some(hook) = &command.identity_hook {
        commands.push(identity_hook_command(
            command,
            hook,
            identity_host.as_deref(),
            host_key_public.as_deref(),
        ));
    }
    commands.push(ssh_command(
        command,
        Phase::Preflight,
        "check remote root access and required tools before staging kexec",
        false,
        [
            "set -euo pipefail",
            "test \"$(id -u)\" = 0",
            "command -v kexec >/dev/null",
            "command -v sshd >/dev/null || command -v systemctl >/dev/null",
            &format!(
                "mkdir -p {}",
                shell_quote(&format!("{}/kexec", command.remote_workdir))
            ),
        ]
        .join("; "),
    ));
    commands.push(scp_to_remote_paths_command(
        command,
        Phase::StageKexec,
        "upload kexec kernel and initrd to the current host",
        false,
        &[
            (&command.kexec_kernel, &remote_kernel),
            (&command.kexec_initrd, &remote_initrd),
        ],
    ));
    commands.push(disconnect_tolerant_ssh_command(
        command,
        Phase::Kexec,
        "load and enter the staged kexec installer",
        true,
        format!(
            "set -euo pipefail; kexec -l {} --initrd={} --append {}; echo 'nixos-kexec: entering kexec' >&2; sync; kexec -e",
            shell_quote(&remote_kernel),
            shell_quote(&remote_initrd),
            shell_quote(&kexec_append)
        ),
    ));
    commands.push(local_command(
        Phase::AwaitInstaller,
        "wait for SSH to return after kexec",
        false,
        vec![
            "sh".to_string(),
            "-c".to_string(),
            await_ssh_script(command),
        ],
    ));
    commands.push(ssh_command(
        command,
        Phase::StageInstall,
        "prepare installer workdir after kexec",
        false,
        format!(
            "set -euo pipefail; command -v tar >/dev/null; mkdir -p {}",
            shell_quote(&command.remote_workdir)
        ),
    ));
    if let Some(flake_source) = &command.flake_source {
        commands.push(stage_flake_source_command(
            command,
            flake_source,
            &remote_flake_source,
        ));
    }
    commands.extend(installer_known_hosts_commands(command));
    commands.push(scp_command(
        command,
        Phase::StageInstall,
        "upload disk-nix install spec to the kexec installer",
        false,
        vec![
            command.disk_spec.display().to_string(),
            format!("{}:{remote_spec}", command.target),
        ],
    ));
    commands.push(ssh_command(
        command,
        Phase::DiskNixApply,
        "apply disk-nix storage spec inside the installer environment",
        true,
        format!(
            "set -euo pipefail; {}",
            disk_nix_invocation(
                command,
                &[
                    "apply".to_string(),
                    "--spec".to_string(),
                    remote_spec.clone(),
                    "--probe-current".to_string(),
                    "--execute".to_string(),
                ]
            )
        ),
    ));
    if let Some(system) = &command.system {
        commands.push(ssh_command(
            command,
            Phase::NixosInstall,
            "mount target storage through disk-nix",
            true,
            format!(
                "set -euo pipefail; {}",
                disk_nix_invocation(
                    command,
                    &[
                        "install".to_string(),
                        "mount".to_string(),
                        "--spec".to_string(),
                        remote_spec.clone(),
                        "--target".to_string(),
                        command.target_root.clone(),
                        "--execute".to_string(),
                    ]
                )
            ),
        ));
        commands.extend(host_identity_commands(command, host_key_public.as_deref()));
        commands.push(copy_system_command(command, system));
        commands.push(ssh_command(
            command,
            Phase::NixosInstall,
            "install prebuilt NixOS system closure",
            true,
            format!(
                "set -euo pipefail; nixos-install --root {} --system {} --no-root-passwd --no-channel-copy",
                shell_quote(&command.target_root),
                shell_quote(&system.display().to_string())
            ),
        ));
    } else if has_host_identity(command) {
        commands.push(ssh_command(
            command,
            Phase::NixosInstall,
            "mount target storage through disk-nix",
            true,
            format!(
                "set -euo pipefail; {}",
                disk_nix_invocation(
                    command,
                    &[
                        "install".to_string(),
                        "mount".to_string(),
                        "--spec".to_string(),
                        remote_spec.clone(),
                        "--target".to_string(),
                        command.target_root.clone(),
                        "--execute".to_string(),
                    ]
                )
            ),
        ));
        commands.extend(host_identity_commands(command, host_key_public.as_deref()));
        commands.push(ssh_command(
            command,
            Phase::NixosInstall,
            "install NixOS flake after provisioning host identity",
            true,
            format!(
                "set -euo pipefail; nixos-install --root {} --flake {} --no-root-passwd --no-channel-copy",
                shell_quote(&command.target_root),
                shell_quote(&install_flake)
            ),
        ));
    } else {
        commands.push(ssh_command(
            command,
            Phase::NixosInstall,
            "mount target storage and run nixos-install through disk-nix",
            true,
            format!(
                "set -euo pipefail; {}",
                disk_nix_invocation(
                    command,
                    &[
                        "install".to_string(),
                        "nixos".to_string(),
                        "--spec".to_string(),
                        remote_spec.clone(),
                        "--flake".to_string(),
                        install_flake.clone(),
                        "--target".to_string(),
                        command.target_root.clone(),
                        "--execute".to_string(),
                    ]
                )
            ),
        ));
    }
    if command.identity_rollback_hook.is_some() {
        commands.push(disarm_identity_rollback_command());
    }
    if !command.no_final_reboot {
        commands.push(ssh_command(
            command,
            Phase::Reboot,
            "sync and reboot into the installed system",
            true,
            "set -euo pipefail; sync; reboot".to_string(),
        ));
    }

    let mut warnings = vec![
        "kexec replaces the running kernel immediately; keep console or power access available"
            .to_string(),
        "disk-nix apply and nixos-install are destructive when the install spec formats disks"
            .to_string(),
        "the kexec installer environment must boot with SSH, Nix, kexec-tools, and network access"
            .to_string(),
        install_strategy_warning(install_strategy).to_string(),
    ];
    if command.host_key.is_some() {
        warnings.push(
            "host keys passed with --host-key are deploy-time secrets; keep private keys outside flakes and the Nix store"
                .to_string(),
        );
    }
    if command.generate_host_key {
        warnings.push(
            "generated host keys are temporary local secrets; the rendered script removes the local private key on success or failure"
                .to_string(),
        );
    }
    if command.identity_hook.is_some() {
        warnings.push(
            "identity hooks can mutate local secret metadata; hooks must be idempotent and should not print private key material"
                .to_string(),
        );
    }
    if command.identity_rollback_hook.is_some() {
        warnings.push(
            "identity rollback hooks run on deployment failure and should undo external side effects created by the identity hook"
                .to_string(),
        );
    }

    Ok(DeploymentPlan {
        target: command.target.clone(),
        flake: command.flake.clone(),
        install_flake,
        install_strategy,
        install_strategy_summary: install_strategy_summary.to_string(),
        flake_source: command
            .flake_source
            .as_ref()
            .map(|path| path.display().to_string()),
        installer_known_hosts_files: command
            .installer_known_hosts_files
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        system: command
            .system
            .as_ref()
            .map(|path| path.display().to_string()),
        disk_spec: command.disk_spec.display().to_string(),
        disk_nix: command.disk_nix.clone(),
        disk_nix_command: command.disk_nix_command.clone(),
        host_key: command
            .host_key
            .as_ref()
            .map(|path| path.display().to_string()),
        host_key_public: host_key_public.map(|path| path.display().to_string()),
        generate_host_key: command.generate_host_key,
        identity_hook: command.identity_hook.clone(),
        identity_rollback_hook: command.identity_rollback_hook.clone(),
        identity_host,
        ssh_tty: command.ssh_tty,
        target_root: command.target_root.clone(),
        remote_workdir: command.remote_workdir.clone(),
        commands,
        warnings,
    })
}

pub fn render_script(plan: &DeploymentPlan) -> String {
    let mut script = String::from(
        "#!/usr/bin/env bash\nset -euo pipefail\n\n# Generated by nixos-kexec. Review before running.\n\n",
    );
    script.push_str("# INSTALL STRATEGY: ");
    script.push_str(&plan.install_strategy_summary);
    script.push('\n');
    for warning in &plan.warnings {
        script.push_str("# WARNING: ");
        script.push_str(warning);
        script.push('\n');
    }
    for command in &plan.commands {
        script.push_str(&format!(
            "\n# {:?}: {}\n",
            command.phase, command.description
        ));
        script.push_str(&render_command(command));
        script.push('\n');
    }
    script
}

fn print_plan(output: &mut impl Write, plan: &DeploymentPlan) -> Result<(), AppError> {
    writeln!(output, "target: {}", plan.target)?;
    writeln!(output, "flake: {}", plan.flake)?;
    if plan.install_flake != plan.flake {
        writeln!(output, "install flake: {}", plan.install_flake)?;
    }
    writeln!(
        output,
        "install strategy: {}",
        plan.install_strategy_summary
    )?;
    if let Some(source) = &plan.flake_source {
        writeln!(output, "flake source: {source}")?;
    }
    for path in &plan.installer_known_hosts_files {
        writeln!(output, "installer known_hosts file: {path}")?;
    }
    if let Some(system) = &plan.system {
        writeln!(output, "system: {system}")?;
    }
    writeln!(output, "disk spec: {}", plan.disk_spec)?;
    writeln!(output, "disk-nix: {}", plan.disk_nix)?;
    if let Some(command) = &plan.disk_nix_command {
        writeln!(output, "disk-nix command: {command}")?;
    }
    if let Some(host_key) = &plan.host_key {
        writeln!(output, "host key: {host_key}")?;
    }
    if let Some(host_key_public) = &plan.host_key_public {
        writeln!(output, "host key public: {host_key_public}")?;
    }
    if plan.generate_host_key {
        writeln!(output, "host key: generated temporary ed25519 key")?;
    }
    if let Some(identity_host) = &plan.identity_host {
        writeln!(output, "identity host: {identity_host}")?;
    }
    if let Some(identity_hook) = &plan.identity_hook {
        writeln!(output, "identity hook: {identity_hook}")?;
    }
    if let Some(identity_rollback_hook) = &plan.identity_rollback_hook {
        writeln!(output, "identity rollback hook: {identity_rollback_hook}")?;
    }
    writeln!(output)?;
    for warning in &plan.warnings {
        writeln!(output, "warning: {warning}")?;
    }
    writeln!(output)?;
    for command in &plan.commands {
        let mutates = if command.mutates {
            "mutates"
        } else {
            "read-only"
        };
        writeln!(
            output,
            "[{:?}] {mutates}: {}",
            command.phase, command.description
        )?;
        writeln!(output, "  {}", render_command(command))?;
    }
    Ok(())
}

fn print_installer_plan(
    output: &mut impl Write,
    plan: &InstallerBuildPlan,
) -> Result<(), AppError> {
    writeln!(output, "installer system: {}", plan.system)?;
    writeln!(output, "nixos-kexec flake: {}", plan.nixos_kexec_flake)?;
    writeln!(output, "disk-nix flake: {}", plan.disk_nix_flake)?;
    writeln!(output, "nixpkgs flake: {}", plan.nixpkgs_flake)?;
    writeln!(output, "authorized keys: {}", plan.authorized_key_count)?;
    if let Some(path) = &plan.network_manager_profiles_json {
        writeln!(output, "NetworkManager profiles JSON: {path}")?;
    }
    if let Some(out_link) = &plan.out_link {
        writeln!(output, "out link: {out_link}")?;
    }
    writeln!(output)?;
    for warning in &plan.warnings {
        writeln!(output, "warning: {warning}")?;
    }
    writeln!(output)?;
    writeln!(output, "{}", shell_command(&plan.argv))?;
    Ok(())
}

fn write_or_print_script(
    output: &mut impl Write,
    path: Option<&Path>,
    script: &str,
) -> Result<(), AppError> {
    if let Some(path) = path {
        fs::write(path, script)?;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions)?;
        writeln!(output, "wrote {}", path.display())?;
    } else {
        write!(output, "{script}")?;
    }
    Ok(())
}

fn require_file(name: &str, path: &Path) -> Result<(), AppError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "{name} does not exist or is not a regular file: {}",
            path.display()
        )))
    }
}

fn require_existing_path(name: &str, path: &Path) -> Result<(), AppError> {
    if path.exists() {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "{name} does not exist: {}",
            path.display()
        )))
    }
}

fn read_authorized_keys(command: &InstallerBuildCommand) -> Result<Vec<String>, AppError> {
    let mut keys = Vec::new();
    keys.extend(command.authorized_keys.iter().cloned());
    for path in &command.authorized_key_files {
        let content = fs::read_to_string(path).map_err(|error| {
            AppError::Message(format!(
                "could not read authorized key file {}: {error}",
                path.display()
            ))
        })?;
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                keys.push(trimmed.to_string());
            }
        }
    }
    Ok(keys)
}

fn installer_expression(command: &InstallerBuildCommand, authorized_keys: &[String]) -> String {
    format!(
        r#"let
  nixosKexec = builtins.getFlake {nixos_kexec};
  nixpkgs = builtins.getFlake {nixpkgs};
  disk-nix = builtins.getFlake {disk_nix};
  installer = import (nixosKexec + "/examples/kexec-installer.nix") {{
    inherit disk-nix nixpkgs;
    authorizedKeys = {authorized_keys};
    networkManagerProfiles = {network_manager_profiles};
    system = {system};
  }};
in
installer.config.system.build.kexecTree"#,
        nixos_kexec = nix_string(&command.nixos_kexec_flake),
        nixpkgs = nix_string(&command.nixpkgs_flake),
        disk_nix = nix_string(&command.disk_nix_flake),
        authorized_keys = nix_list(authorized_keys),
        network_manager_profiles = installer_network_manager_profiles(command),
        system = nix_string(&command.system),
    )
}

fn installer_network_manager_profiles(command: &InstallerBuildCommand) -> String {
    command
        .network_manager_profiles_json
        .as_ref()
        .map(|path| {
            format!(
                "builtins.fromJSON (builtins.readFile {})",
                nix_string(&path.display().to_string())
            )
        })
        .unwrap_or_else(|| "{ }".to_string())
}

fn nix_list(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| nix_string(value))
        .collect::<Vec<_>>()
        .join(" ");
    format!("[ {items} ]")
}

fn nix_string(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace("${", "\\${");
    format!("\"{escaped}\"")
}

fn ssh_command(
    command: &SshCommand,
    phase: Phase,
    description: &str,
    mutates: bool,
    remote_script: String,
) -> PlanCommand {
    let mut argv = vec!["ssh".to_string()];
    if command.ssh_tty && mutates {
        argv.push("-tt".to_string());
    }
    argv.extend(
        command
            .ssh_options
            .iter()
            .flat_map(|option| ["-o".to_string(), option.clone()]),
    );
    argv.push(command.target.clone());
    argv.push(remote_script);
    local_command(phase, description, mutates, argv)
}

fn disconnect_tolerant_ssh_command(
    command: &SshCommand,
    phase: Phase,
    description: &str,
    mutates: bool,
    remote_script: String,
) -> PlanCommand {
    let ssh = shell_command(
        &["timeout".to_string(), "120s".to_string(), "ssh".to_string()]
            .into_iter()
            .chain(
                command
                    .ssh_options
                    .iter()
                    .flat_map(|option| ["-o".to_string(), option.clone()]),
            )
            .chain([command.target.clone(), remote_script])
            .collect::<Vec<_>>(),
    );
    local_command(
        phase,
        description,
        mutates,
        vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "set +e; output=$({ssh} 2>&1); status=$?; set -e; printf '%s\\n' \"$output\"; if [ \"$status\" -eq 0 ]; then exit 0; fi; if [ \"$status\" -eq 255 ] || [ \"$status\" -eq 124 ]; then case \"$output\" in *'nixos-kexec: entering kexec'*|*'Connection to '*' closed.'*) exit 0 ;; esac; fi; exit \"$status\""
            ),
        ],
    )
}

fn scp_command(
    command: &SshCommand,
    phase: Phase,
    description: &str,
    mutates: bool,
    args: Vec<String>,
) -> PlanCommand {
    let mut argv = vec!["scp".to_string()];
    argv.extend(
        command
            .ssh_options
            .iter()
            .flat_map(|option| ["-o".to_string(), option.clone()]),
    );
    argv.extend(args);
    local_command(phase, description, mutates, argv)
}

fn scp_to_remote_paths_command(
    command: &SshCommand,
    phase: Phase,
    description: &str,
    mutates: bool,
    files: &[(&PathBuf, &str)],
) -> PlanCommand {
    let commands = files
        .iter()
        .map(|(local_path, remote_path)| {
            shell_command(
                &std::iter::once("scp".to_string())
                    .chain(
                        command
                            .ssh_options
                            .iter()
                            .flat_map(|option| ["-o".to_string(), option.clone()]),
                    )
                    .chain([
                        local_path.display().to_string(),
                        format!("{}:{remote_path}", command.target),
                    ])
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>()
        .join(" && ");
    local_command(
        phase,
        description,
        mutates,
        vec!["sh".to_string(), "-c".to_string(), commands],
    )
}

fn stage_flake_source_command(
    command: &SshCommand,
    flake_source: &Path,
    remote_flake_source: &str,
) -> PlanCommand {
    let remote_script = format!(
        "set -euo pipefail; rm -rf {}; mkdir -p {}; tar -xzf - -C {}",
        shell_quote(remote_flake_source),
        shell_quote(remote_flake_source),
        shell_quote(remote_flake_source)
    );
    let remote = shell_command(
        &std::iter::once("ssh".to_string())
            .chain(
                command
                    .ssh_options
                    .iter()
                    .flat_map(|option| ["-o".to_string(), option.clone()]),
            )
            .chain([command.target.clone(), remote_script])
            .collect::<Vec<_>>(),
    );
    let local = shell_command(&[
        "tar".to_string(),
        "-C".to_string(),
        flake_source.display().to_string(),
        "--exclude".to_string(),
        ".git".to_string(),
        "--exclude".to_string(),
        ".jj".to_string(),
        "-czf".to_string(),
        "-".to_string(),
        ".".to_string(),
    ]);
    local_command(
        Phase::StageInstall,
        "upload local flake source into the installer",
        false,
        vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("{local} | {remote}"),
        ],
    )
}

fn installer_known_hosts_commands(command: &SshCommand) -> Vec<PlanCommand> {
    if command.installer_known_hosts_files.is_empty() {
        return Vec::new();
    }

    let remote_paths = command
        .installer_known_hosts_files
        .iter()
        .enumerate()
        .map(|(index, _)| format!("{}/installer-known-hosts-{index}", command.remote_workdir))
        .collect::<Vec<_>>();
    let mut commands = command
        .installer_known_hosts_files
        .iter()
        .zip(remote_paths.iter())
        .map(|(local_path, remote_path)| {
            upload_file_via_ssh_stdin_command(
                command,
                Phase::StageInstall,
                "upload SSH known_hosts for installer-side flake fetches",
                local_path,
                remote_path,
                "0644",
            )
        })
        .collect::<Vec<_>>();

    let remote_known_hosts = format!("{}/installer-known-hosts", command.remote_workdir);
    let remote_inputs = remote_paths
        .iter()
        .map(|path| shell_quote(path))
        .collect::<Vec<_>>()
        .join(" ");
    let cleanup_paths = remote_paths
        .iter()
        .map(|path| shell_quote(path))
        .chain([shell_quote(&remote_known_hosts)])
        .collect::<Vec<_>>()
        .join(" ");
    commands.push(ssh_command(
        command,
        Phase::StageInstall,
        "install SSH known_hosts into the kexec installer",
        false,
        format!(
            "set -euo pipefail; install -d -m 0700 /root/.ssh; install -d -m 0755 /etc/ssh; cat {remote_inputs} > {}; install -o root -g root -m 0644 {} /root/.ssh/known_hosts; install -o root -g root -m 0644 {} /etc/ssh/ssh_known_hosts; rm -f {cleanup_paths}",
            shell_quote(&remote_known_hosts),
            shell_quote(&remote_known_hosts),
            shell_quote(&remote_known_hosts),
        ),
    ));

    commands
}

fn effective_host_key_public(command: &SshCommand) -> Result<Option<PathBuf>, AppError> {
    if let Some(path) = &command.host_key_public {
        require_file("host public key", path)?;
        return Ok(Some(path.clone()));
    }
    let Some(host_key) = &command.host_key else {
        return Ok(None);
    };
    let public_key = append_pub_suffix(host_key);
    if public_key.is_file() {
        Ok(Some(public_key))
    } else {
        Ok(None)
    }
}

fn append_pub_suffix(path: &Path) -> PathBuf {
    let mut public_key = path.as_os_str().to_os_string();
    public_key.push(".pub");
    PathBuf::from(public_key)
}

fn has_host_identity(command: &SshCommand) -> bool {
    command.host_key.is_some() || command.generate_host_key
}

fn effective_identity_host(command: &SshCommand) -> Result<Option<String>, AppError> {
    if let Some(host) = &command.identity_host {
        let trimmed = host.trim();
        if trimmed.is_empty() {
            return Err(AppError::Message(
                "--identity-host cannot be empty".to_string(),
            ));
        }
        return Ok(Some(trimmed.to_string()));
    }
    if command.identity_hook.is_none() && !command.generate_host_key {
        return Ok(None);
    }
    Ok(command
        .flake
        .split_once('#')
        .and_then(|(_, fragment)| (!fragment.is_empty()).then_some(fragment.to_string())))
}

fn generate_host_key_command(_command: &SshCommand, identity_host: Option<&str>) -> PlanCommand {
    let comment = identity_host
        .map(|host| format!("root@{host} bootstrap host key"))
        .unwrap_or_else(|| "nixos-kexec bootstrap host key".to_string());
    let script = format!(
        "NIXOS_KEXEC_HOST_KEY_DIR=$(mktemp -d \"${{TMPDIR:-/tmp}}/nixos-kexec-host-key.XXXXXX\"); export NIXOS_KEXEC_HOST_KEY_DIR; NIXOS_KEXEC_IDENTITY_STATE_DIR=\"$NIXOS_KEXEC_HOST_KEY_DIR\"; export NIXOS_KEXEC_IDENTITY_STATE_DIR; chmod 700 \"$NIXOS_KEXEC_HOST_KEY_DIR\"; cleanup_nixos_kexec_host_key() {{ if [ -n \"${{NIXOS_KEXEC_HOST_KEY_DIR:-}}\" ]; then rm -rf -- \"$NIXOS_KEXEC_HOST_KEY_DIR\"; fi; }}; trap cleanup_nixos_kexec_host_key EXIT; NIXOS_KEXEC_HOST_KEY=\"$NIXOS_KEXEC_HOST_KEY_DIR/ssh_host_ed25519_key\"; NIXOS_KEXEC_HOST_KEY_PUBLIC=\"$NIXOS_KEXEC_HOST_KEY.pub\"; export NIXOS_KEXEC_HOST_KEY NIXOS_KEXEC_HOST_KEY_PUBLIC; ssh-keygen -q -t ed25519 -N '' -C {} -f \"$NIXOS_KEXEC_HOST_KEY\"; chmod 600 \"$NIXOS_KEXEC_HOST_KEY\"; chmod 644 \"$NIXOS_KEXEC_HOST_KEY_PUBLIC\"",
        shell_quote(&comment)
    );
    local_command(
        Phase::StageHostIdentity,
        "generate temporary SSH host key for target install",
        true,
        raw_script_argv(script),
    )
}

fn identity_state_command() -> PlanCommand {
    local_command(
        Phase::StageHostIdentity,
        "create temporary identity hook state directory",
        true,
        raw_script_argv(
            "NIXOS_KEXEC_IDENTITY_STATE_DIR=$(mktemp -d \"${TMPDIR:-/tmp}/nixos-kexec-identity.XXXXXX\"); export NIXOS_KEXEC_IDENTITY_STATE_DIR; chmod 700 \"$NIXOS_KEXEC_IDENTITY_STATE_DIR\"; cleanup_nixos_kexec_identity_state() { if [ -n \"${NIXOS_KEXEC_IDENTITY_STATE_DIR:-}\" ]; then rm -rf -- \"$NIXOS_KEXEC_IDENTITY_STATE_DIR\"; fi; }; trap cleanup_nixos_kexec_identity_state EXIT".to_string(),
        ),
    )
}

fn identity_hook_command(
    command: &SshCommand,
    hook: &str,
    identity_host: Option<&str>,
    static_public_key: Option<&Path>,
) -> PlanCommand {
    let script = format!(
        "set -euo pipefail; {}; NIXOS_KEXEC_IDENTITY_HOOK_EVENT=apply; export NIXOS_KEXEC_IDENTITY_HOOK_EVENT; {hook}",
        identity_environment_script(command, identity_host, static_public_key),
        hook = hook,
    );
    local_command(
        Phase::StageHostIdentity,
        "run host identity hook",
        true,
        vec!["sh".to_string(), "-c".to_string(), script],
    )
}

fn identity_rollback_trap_command(
    command: &SshCommand,
    rollback_hook: &str,
    identity_host: Option<&str>,
    static_public_key: Option<&Path>,
) -> PlanCommand {
    let script = format!(
        "set -euo pipefail; {}; nixos_kexec_identity_rollback() {{ status=$?; if [ \"$status\" -ne 0 ] && [ \"${{NIXOS_KEXEC_IDENTITY_ROLLBACK_ARMED:-0}}\" = 1 ]; then echo 'nixos-kexec: running identity rollback hook after deployment failure' >&2; NIXOS_KEXEC_IDENTITY_HOOK_EVENT=rollback; export NIXOS_KEXEC_IDENTITY_HOOK_EVENT; {rollback_hook} || echo 'nixos-kexec: identity rollback hook failed' >&2; fi; if command -v cleanup_nixos_kexec_host_key >/dev/null 2>&1; then cleanup_nixos_kexec_host_key; fi; if command -v cleanup_nixos_kexec_identity_state >/dev/null 2>&1; then cleanup_nixos_kexec_identity_state; fi; exit \"$status\"; }}; NIXOS_KEXEC_IDENTITY_ROLLBACK_ARMED=1; export NIXOS_KEXEC_IDENTITY_ROLLBACK_ARMED; trap nixos_kexec_identity_rollback EXIT",
        identity_environment_script(command, identity_host, static_public_key),
        rollback_hook = rollback_hook,
    );
    local_command(
        Phase::StageHostIdentity,
        "arm identity rollback hook",
        true,
        raw_script_argv(script),
    )
}

fn disarm_identity_rollback_command() -> PlanCommand {
    local_command(
        Phase::StageHostIdentity,
        "disarm identity rollback hook after successful deployment",
        true,
        raw_script_argv("NIXOS_KEXEC_IDENTITY_ROLLBACK_ARMED=0".to_string()),
    )
}

fn identity_environment_script(
    command: &SshCommand,
    identity_host: Option<&str>,
    static_public_key: Option<&Path>,
) -> String {
    let public_key_file = if command.generate_host_key {
        GENERATED_HOST_KEY_PUBLIC_VAR.to_string()
    } else {
        static_public_key
            .map(|path| shell_quote(&path.display().to_string()))
            .unwrap_or_else(|| "".to_string())
    };
    format!(
        "NIXOS_KEXEC_IDENTITY_HOST={identity_host}; export NIXOS_KEXEC_IDENTITY_HOST; NIXOS_KEXEC_IDENTITY_STATE_DIR=\"${{NIXOS_KEXEC_IDENTITY_STATE_DIR:-}}\"; export NIXOS_KEXEC_IDENTITY_STATE_DIR; NIXOS_KEXEC_SSH_PUBLIC_KEY_FILE={public_key_file}; export NIXOS_KEXEC_SSH_PUBLIC_KEY_FILE; if [ -n \"$NIXOS_KEXEC_SSH_PUBLIC_KEY_FILE\" ] && [ -f \"$NIXOS_KEXEC_SSH_PUBLIC_KEY_FILE\" ]; then NIXOS_KEXEC_SSH_PUBLIC_KEY=$(cat \"$NIXOS_KEXEC_SSH_PUBLIC_KEY_FILE\"); else NIXOS_KEXEC_SSH_PUBLIC_KEY=''; fi; export NIXOS_KEXEC_SSH_PUBLIC_KEY; if [ -n \"$NIXOS_KEXEC_SSH_PUBLIC_KEY\" ] && command -v ssh-to-age >/dev/null 2>&1; then NIXOS_KEXEC_AGE_RECIPIENT=$(printf '%s\\n' \"$NIXOS_KEXEC_SSH_PUBLIC_KEY\" | ssh-to-age 2>/dev/null || true); else NIXOS_KEXEC_AGE_RECIPIENT=''; fi; export NIXOS_KEXEC_AGE_RECIPIENT",
        identity_host = shell_quote(identity_host.unwrap_or("")),
        public_key_file = public_key_file,
    )
}

fn host_identity_commands(command: &SshCommand, public_key: Option<&Path>) -> Vec<PlanCommand> {
    if !has_host_identity(command) {
        return Vec::new();
    }

    let remote_dir = format!("{}/host-identity", command.remote_workdir);
    let remote_private_key = format!("{remote_dir}/ssh_host_ed25519_key");
    let remote_public_key = format!("{remote_private_key}.pub");
    let target_ssh_dir = format!("{}/etc/ssh", command.target_root);
    let target_private_key = format!("{target_ssh_dir}/ssh_host_ed25519_key");
    let target_public_key = format!("{target_private_key}.pub");

    let mut commands = Vec::new();
    if command.generate_host_key {
        commands.push(upload_variable_file_via_ssh_stdin_command(
            command,
            Phase::StageHostIdentity,
            "upload generated private SSH host key into the installer workdir",
            GENERATED_HOST_KEY_VAR,
            &remote_private_key,
            "0600",
        ));
        commands.push(upload_variable_file_via_ssh_stdin_command(
            command,
            Phase::StageHostIdentity,
            "upload generated public SSH host key into the installer workdir",
            GENERATED_HOST_KEY_PUBLIC_VAR,
            &remote_public_key,
            "0644",
        ));
    } else if let Some(private_key) = &command.host_key {
        commands.push(upload_file_via_ssh_stdin_command(
            command,
            Phase::StageHostIdentity,
            "upload private SSH host key into the installer workdir",
            private_key,
            &remote_private_key,
            "0600",
        ));
        if let Some(public_key) = public_key {
            commands.push(upload_file_via_ssh_stdin_command(
                command,
                Phase::StageHostIdentity,
                "upload public SSH host key into the installer workdir",
                public_key,
                &remote_public_key,
                "0644",
            ));
        }
    }

    let mut install_script = format!(
        "set -euo pipefail; install -d -m 0755 {}; install -o root -g root -m 0600 {} {}",
        shell_quote(&target_ssh_dir),
        shell_quote(&remote_private_key),
        shell_quote(&target_private_key)
    );
    if command.generate_host_key || public_key.is_some() {
        install_script.push_str(&format!(
            "; install -o root -g root -m 0644 {} {}",
            shell_quote(&remote_public_key),
            shell_quote(&target_public_key)
        ));
    }
    install_script.push_str(&format!(
        "; test \"$(stat -c '%a' {})\" = 600; test \"$(stat -c '%U:%G' {})\" = root:root",
        shell_quote(&target_private_key),
        shell_quote(&target_private_key)
    ));
    install_script.push_str(&format!("; rm -rf {}", shell_quote(&remote_dir)));
    commands.push(ssh_command(
        command,
        Phase::StageHostIdentity,
        "install SSH host key into the mounted target",
        true,
        install_script,
    ));

    commands
}

fn upload_file_via_ssh_stdin_command(
    command: &SshCommand,
    phase: Phase,
    description: &str,
    local_path: &Path,
    remote_path: &str,
    mode: &str,
) -> PlanCommand {
    let remote_dir = Path::new(remote_path)
        .parent()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| ".".to_string());
    let remote_script = format!(
        "set -euo pipefail; umask 077; mkdir -p {}; cat > {}; chmod {} {}",
        shell_quote(&remote_dir),
        shell_quote(remote_path),
        shell_quote(mode),
        shell_quote(remote_path)
    );
    let ssh = shell_command(
        &std::iter::once("ssh".to_string())
            .chain(
                command
                    .ssh_options
                    .iter()
                    .flat_map(|option| ["-o".to_string(), option.clone()]),
            )
            .chain([command.target.clone(), remote_script])
            .collect::<Vec<_>>(),
    );
    local_command(
        phase,
        description,
        true,
        vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("{ssh} < {}", shell_quote(&local_path.display().to_string())),
        ],
    )
}

fn upload_variable_file_via_ssh_stdin_command(
    command: &SshCommand,
    phase: Phase,
    description: &str,
    local_path_expr: &str,
    remote_path: &str,
    mode: &str,
) -> PlanCommand {
    let remote_dir = Path::new(remote_path)
        .parent()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| ".".to_string());
    let remote_script = format!(
        "set -euo pipefail; umask 077; mkdir -p {}; cat > {}; chmod {} {}",
        shell_quote(&remote_dir),
        shell_quote(remote_path),
        shell_quote(mode),
        shell_quote(remote_path)
    );
    let ssh = shell_command(
        &std::iter::once("ssh".to_string())
            .chain(
                command
                    .ssh_options
                    .iter()
                    .flat_map(|option| ["-o".to_string(), option.clone()]),
            )
            .chain([command.target.clone(), remote_script])
            .collect::<Vec<_>>(),
    );
    local_command(
        phase,
        description,
        true,
        vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("{ssh} < \"{local_path_expr}\""),
        ],
    )
}

fn copy_system_command(command: &SshCommand, system: &Path) -> PlanCommand {
    let remote_store = format!("local?root={}", command.target_root);
    let store_uri = format!(
        "ssh-ng://{}?remote-store={}",
        command.target,
        percent_encode_query_value(&remote_store)
    );
    let nix_copy = shell_command(&[
        "nix".to_string(),
        "copy".to_string(),
        "--no-check-sigs".to_string(),
        "--to".to_string(),
        store_uri,
        system.display().to_string(),
    ]);
    let script = if command.ssh_options.is_empty() {
        nix_copy
    } else {
        format!(
            "NIX_SSHOPTS={} {nix_copy}",
            shell_quote(&ssh_options_for_nix(&command.ssh_options))
        )
    };
    local_command(
        Phase::CopySystem,
        "copy prebuilt NixOS system closure into the mounted target store",
        true,
        vec!["sh".to_string(), "-c".to_string(), script],
    )
}

fn local_command(phase: Phase, description: &str, mutates: bool, argv: Vec<String>) -> PlanCommand {
    PlanCommand {
        phase,
        argv,
        description: description.to_string(),
        mutates,
    }
}

fn raw_script_argv(script: String) -> Vec<String> {
    vec![RAW_SCRIPT_SENTINEL.to_string(), script]
}

fn render_command(command: &PlanCommand) -> String {
    if command
        .argv
        .first()
        .is_some_and(|arg| arg == RAW_SCRIPT_SENTINEL)
    {
        command.argv.get(1).cloned().unwrap_or_default()
    } else {
        shell_command(&command.argv)
    }
}

fn await_ssh_script(command: &SshCommand) -> String {
    let ssh = shell_command(
        &std::iter::once("timeout".to_string())
            .chain(["15s".to_string(), "ssh".to_string()])
            .chain(
                command
                    .ssh_options
                    .iter()
                    .flat_map(|option| ["-o".to_string(), option.clone()]),
            )
            .chain([
                "-o".to_string(),
                "ConnectTimeout=5".to_string(),
                command.target.clone(),
                "true".to_string(),
            ])
            .collect::<Vec<_>>(),
    );
    format!(
        "deadline=$(($(date +%s) + 600)); while :; do if {ssh}; then break; fi; if [ \"$(date +%s)\" -ge \"$deadline\" ]; then echo 'timed out waiting for installer SSH' >&2; exit 1; fi; sleep 5; done"
    )
}

fn install_flake(command: &SshCommand, remote_flake_source: &str) -> Result<String, AppError> {
    if command.flake_source.is_none() {
        return Ok(command.flake.clone());
    }
    let Some((_, fragment)) = command.flake.split_once('#') else {
        return Err(AppError::Message(
            "local flake source staging requires --flake to include a #host fragment".to_string(),
        ));
    };
    if fragment.is_empty() {
        return Err(AppError::Message(
            "local flake source staging requires a non-empty #host fragment".to_string(),
        ));
    }
    Ok(format!("path:{remote_flake_source}#{fragment}"))
}

fn install_strategy(command: &SshCommand) -> InstallStrategy {
    if command.system.is_some() {
        InstallStrategy::CopyPrebuiltSystem
    } else if command.flake_source.is_some() {
        InstallStrategy::StageLocalFlake
    } else {
        InstallStrategy::InstallerBuildsFlake
    }
}

fn install_strategy_summary(strategy: InstallStrategy) -> &'static str {
    match strategy {
        InstallStrategy::InstallerBuildsFlake => {
            "installer evaluates the flake and builds or downloads the target system closure"
        }
        InstallStrategy::StageLocalFlake => {
            "nixos-kexec uploads local flake source; installer builds or downloads the target system closure"
        }
        InstallStrategy::CopyPrebuiltSystem => {
            "nixos-kexec copies a prebuilt system closure into the mounted target store"
        }
    }
}

fn install_strategy_warning(strategy: InstallStrategy) -> &'static str {
    match strategy {
        InstallStrategy::InstallerBuildsFlake => {
            "no system closure is copied by nixos-kexec; the installer must be able to fetch, build, or substitute the flake"
        }
        InstallStrategy::StageLocalFlake => {
            "local flake source is uploaded after kexec; the installer still must build or substitute the system closure"
        }
        InstallStrategy::CopyPrebuiltSystem => {
            "--system copies the prebuilt closure after disk-nix mounts the target; this can be large over slow links"
        }
    }
}

fn effective_kexec_append(command: &SshCommand) -> Result<String, AppError> {
    if let Some(kexec_append) = &command.kexec_append {
        return Ok(kexec_append.clone());
    }
    let Some(kexec_tree) = command.kexec_kernel.parent() else {
        return Err(AppError::Message(
            "could not infer kexec command line; pass --kexec-append explicitly".to_string(),
        ));
    };
    let kexec_boot = kexec_tree.join("kexec-boot");
    let script = fs::read_to_string(&kexec_boot).map_err(|error| {
        AppError::Message(format!(
            "could not infer kexec command line from {}: {error}; pass --kexec-append explicitly",
            kexec_boot.display()
        ))
    })?;
    parse_kexec_boot_command_line(&script).ok_or_else(|| {
        AppError::Message(format!(
            "could not find --command-line in {}; pass --kexec-append explicitly",
            kexec_boot.display()
        ))
    })
}

fn parse_kexec_boot_command_line(script: &str) -> Option<String> {
    let marker = "--command-line";
    let marker_start = script.find(marker)?;
    let rest = script[marker_start + marker.len()..].trim_start();
    let mut chars = rest.chars();
    let quote = chars.next()?;
    if quote != '"' && quote != '\'' {
        return rest
            .split_whitespace()
            .next()
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
    }

    let mut value = String::new();
    let mut escaped = false;
    for character in chars {
        if escaped {
            value.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            return Some(value);
        } else {
            value.push(character);
        }
    }
    None
}

fn disk_nix_invocation(command: &SshCommand, args: &[String]) -> String {
    let mut argv = if let Some(disk_nix_command) = &command.disk_nix_command {
        vec![disk_nix_command.clone()]
    } else {
        vec![
            "nix".to_string(),
            "--extra-experimental-features".to_string(),
            "nix-command flakes".to_string(),
            "run".to_string(),
            command.disk_nix.clone(),
            "--".to_string(),
        ]
    };
    argv.extend(args.iter().cloned());
    shell_command(&argv)
}

fn ssh_options_for_nix(options: &[String]) -> String {
    options
        .iter()
        .flat_map(|option| ["-o".to_string(), option.clone()])
        .collect::<Vec<_>>()
        .join(" ")
}

fn percent_encode_query_value(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn shell_command(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    if value.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '/' | '.' | '_' | '-' | ':' | '=' | '+' | '@' | '%' | ',')
    }) {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_command(temp: &tempfile::TempDir) -> SshCommand {
        let disk_spec = temp.path().join("install.json");
        let kernel = temp.path().join("kernel");
        let initrd = temp.path().join("initrd");
        fs::write(&disk_spec, "{}").unwrap();
        fs::write(&kernel, "kernel").unwrap();
        fs::write(&initrd, "initrd").unwrap();
        SshCommand {
            target: "root@192.0.2.10".to_string(),
            flake: "github:example/flake#host".to_string(),
            flake_source: None,
            installer_known_hosts_files: Vec::new(),
            system: None,
            disk_spec,
            kexec_kernel: kernel,
            kexec_initrd: initrd,
            kexec_append: Some("console=ttyS0".to_string()),
            disk_nix: DEFAULT_DISK_NIX.to_string(),
            disk_nix_command: None,
            host_key: None,
            host_key_public: None,
            generate_host_key: false,
            identity_hook: None,
            identity_rollback_hook: None,
            identity_host: None,
            ssh_tty: false,
            target_root: DEFAULT_TARGET_ROOT.to_string(),
            remote_workdir: DEFAULT_REMOTE_WORKDIR.to_string(),
            ssh_options: vec!["StrictHostKeyChecking=accept-new".to_string()],
            script_out: None,
            json: false,
            no_final_reboot: false,
        }
    }

    fn fixture_installer_command(temp: &tempfile::TempDir) -> InstallerBuildCommand {
        let key_file = temp.path().join("id_ed25519.pub");
        fs::write(
            &key_file,
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITest nixos-kexec-test\n",
        )
        .unwrap();
        InstallerBuildCommand {
            authorized_key_files: vec![key_file],
            authorized_keys: vec!["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDirect direct".to_string()],
            network_manager_profiles_json: None,
            system: "x86_64-linux".to_string(),
            nixos_kexec_flake: DEFAULT_NIXOS_KEXEC_FLAKE.to_string(),
            disk_nix_flake: DEFAULT_DISK_NIX_FLAKE.to_string(),
            nixpkgs_flake: DEFAULT_NIXPKGS_FLAKE.to_string(),
            out_link: Some(temp.path().join("installer-result")),
            json: false,
        }
    }

    #[test]
    fn ssh_plan_contains_kexec_disk_nix_install_and_reboot_phases() {
        let temp = tempfile::tempdir().unwrap();
        let plan = build_plan(&fixture_command(&temp)).unwrap();

        assert_eq!(plan.commands.len(), 9);
        assert_eq!(plan.install_strategy, InstallStrategy::InstallerBuildsFlake);
        assert!(plan
            .install_strategy_summary
            .contains("installer evaluates the flake"));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::Kexec
                && command.mutates
                && command.argv.iter().any(|arg| arg.contains("kexec -l"))
                && command.argv.iter().any(|arg| arg.contains("kexec -e"))
                && !command
                    .argv
                    .iter()
                    .any(|arg| arg.contains("systemctl kexec"))
        }));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::DiskNixApply
                && command
                    .argv
                    .iter()
                    .any(|arg| arg.contains("disk-nix-install.json"))
                && command.argv.iter().any(|arg| arg.contains("apply --spec"))
        }));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::NixosInstall
                && command.argv.iter().any(|arg| arg.contains("install nixos"))
        }));
        assert_eq!(plan.commands.last().unwrap().phase, Phase::Reboot);
    }

    #[test]
    fn rendered_script_keeps_commands_reviewable() {
        let temp = tempfile::tempdir().unwrap();
        let plan = build_plan(&fixture_command(&temp)).unwrap();
        let script = render_script(&plan);

        assert!(script.starts_with("#!/usr/bin/env bash\nset -euo pipefail"));
        assert!(script
            .contains("INSTALL STRATEGY: installer evaluates the flake and builds or downloads"));
        assert!(script.contains("WARNING: kexec replaces the running kernel immediately"));
        assert!(script.contains("WARNING: no system closure is copied by nixos-kexec"));
        assert!(script.contains("ssh -o StrictHostKeyChecking=accept-new root@192.0.2.10"));
        assert!(script.contains("timeout 120s ssh"));
        assert!(script.contains("set +e; output=$(timeout 120s ssh"));
        assert!(script.contains("[ \"$status\" -eq 255 ] || [ \"$status\" -eq 124 ]"));
        assert!(script.contains("while :; do if timeout 15s ssh"));
        assert!(script.contains("sync; kexec -e"));
        assert!(!script.contains("systemctl kexec"));
        assert!(script.contains("nix --extra-experimental-features"));
        assert!(script.contains("github:hermetic-foundation/disk-nix#disk-nix"));
    }

    #[test]
    fn plan_requires_explicit_user_in_ssh_target() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = fixture_command(&temp);
        command.target = "192.0.2.10".to_string();

        let error = build_plan(&command).unwrap_err().to_string();

        assert!(error.contains("SSH target should include a user"));
    }

    #[test]
    fn plan_can_use_preinstalled_disk_nix_command_without_final_reboot() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = fixture_command(&temp);
        command.disk_nix_command = Some("/run/current-system/sw/bin/disk-nix".to_string());
        command.no_final_reboot = true;

        let plan = build_plan(&command).unwrap();

        assert_eq!(plan.commands.len(), 8);
        assert_eq!(plan.commands.last().unwrap().phase, Phase::NixosInstall);
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::DiskNixApply
                && command
                    .argv
                    .iter()
                    .any(|arg| arg.contains("/run/current-system/sw/bin/disk-nix apply"))
        }));
        assert!(plan
            .commands
            .iter()
            .all(|command| command.phase != Phase::Reboot));
    }

    #[test]
    fn ssh_tty_only_applies_to_mutating_ssh_commands() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = fixture_command(&temp);
        command.ssh_tty = true;

        let plan = build_plan(&command).unwrap();

        assert!(plan.ssh_tty);
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::DiskNixApply
                && command.argv.first().is_some_and(|arg| arg == "ssh")
                && command.argv.iter().any(|arg| arg == "-tt")
        }));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::Preflight
                && command.argv.first().is_some_and(|arg| arg == "ssh")
                && !command.argv.iter().any(|arg| arg == "-tt")
        }));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::Kexec && command.argv.iter().all(|arg| !arg.contains("ssh -tt"))
        }));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::AwaitInstaller
                && command.argv.iter().all(|arg| !arg.contains("ssh -tt"))
        }));
    }

    #[test]
    fn local_flake_source_is_staged_and_used_for_install() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("flake.nix"), "{}").unwrap();
        let mut command = fixture_command(&temp);
        command.flake = "path:/home/me/flake#host".to_string();
        command.flake_source = Some(temp.path().to_path_buf());
        command.no_final_reboot = true;

        let plan = build_plan(&command).unwrap();

        assert_eq!(plan.install_strategy, InstallStrategy::StageLocalFlake);
        assert_eq!(
            plan.install_flake,
            "path:/tmp/nixos-kexec/flake-source#host"
        );
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::StageInstall
                && command
                    .argv
                    .iter()
                    .any(|arg| arg.contains("tar -C") && arg.contains("flake-source"))
        }));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::NixosInstall
                && command
                    .argv
                    .iter()
                    .any(|arg| arg.contains("--flake") && arg.contains("flake-source#host"))
        }));
    }

    #[test]
    fn local_flake_source_requires_host_fragment() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = fixture_command(&temp);
        command.flake = "path:/home/me/flake".to_string();
        command.flake_source = Some(temp.path().to_path_buf());

        let error = build_plan(&command).unwrap_err().to_string();

        assert!(error.contains("requires --flake to include a #host fragment"));
    }

    #[test]
    fn installer_known_hosts_are_staged_before_flake_fetches() {
        let temp = tempfile::tempdir().unwrap();
        let known_hosts = temp.path().join("known_hosts");
        fs::write(
            &known_hosts,
            "github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITest\n",
        )
        .unwrap();
        let mut command = fixture_command(&temp);
        command.installer_known_hosts_files = vec![known_hosts.clone()];
        command.no_final_reboot = true;

        let plan = build_plan(&command).unwrap();
        let script = render_script(&plan);

        assert_eq!(
            plan.installer_known_hosts_files,
            vec![known_hosts.display().to_string()]
        );
        assert!(script.contains("installer-known-hosts-0"));
        assert!(script.contains("/root/.ssh/known_hosts"));
        assert!(script.contains("/etc/ssh/ssh_known_hosts"));

        let known_hosts_index = plan
            .commands
            .iter()
            .position(|command| {
                command.description == "install SSH known_hosts into the kexec installer"
            })
            .unwrap();
        let disk_apply_index = plan
            .commands
            .iter()
            .position(|command| command.phase == Phase::DiskNixApply)
            .unwrap();
        let nixos_install_index = plan
            .commands
            .iter()
            .position(|command| command.phase == Phase::NixosInstall)
            .unwrap();

        assert!(known_hosts_index < disk_apply_index);
        assert!(known_hosts_index < nixos_install_index);
    }

    #[test]
    fn installer_known_hosts_file_must_exist() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = fixture_command(&temp);
        command.installer_known_hosts_files = vec![temp.path().join("missing-known-hosts")];

        let error = build_plan(&command).unwrap_err().to_string();

        assert!(error.contains("installer known_hosts file does not exist"));
    }

    #[test]
    fn system_and_flake_source_are_mutually_exclusive_strategies() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("flake.nix"), "{}").unwrap();
        let system = temp.path().join("system");
        fs::create_dir(&system).unwrap();
        let mut command = fixture_command(&temp);
        command.flake_source = Some(temp.path().to_path_buf());
        command.system = Some(system);

        let error = build_plan(&command).unwrap_err().to_string();

        assert!(error.contains("--system and --flake-source select different install strategies"));
    }

    #[test]
    fn kexec_append_is_inferred_from_sibling_kexec_boot() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = fixture_command(&temp);
        command.kexec_append = None;
        fs::write(
            temp.path().join("kexec-boot"),
            r#"kexec --load ./bzImage \
  --initrd=./initrd.gz \
  --command-line "init=/nix/store/example-system/init console=ttyS0 panic=1"
"#,
        )
        .unwrap();

        let plan = build_plan(&command).unwrap();

        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::Kexec
                && command.argv.iter().any(|arg| {
                    arg.contains("init=/nix/store/example-system/init console=ttyS0 panic=1")
                })
        }));
    }

    #[test]
    fn kexec_append_requires_explicit_value_when_no_kexec_boot_exists() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = fixture_command(&temp);
        command.kexec_append = None;

        let error = build_plan(&command).unwrap_err().to_string();

        assert!(error.contains("could not infer kexec command line"));
        assert!(error.contains("pass --kexec-append explicitly"));
    }

    #[test]
    fn prebuilt_system_mounts_copies_and_installs_without_remote_flake_eval() {
        let temp = tempfile::tempdir().unwrap();
        let system = temp.path().join("system");
        fs::create_dir(&system).unwrap();
        let mut command = fixture_command(&temp);
        command.system = Some(system.clone());
        command.no_final_reboot = true;

        let plan = build_plan(&command).unwrap();

        assert_eq!(plan.system, Some(system.display().to_string()));
        assert_eq!(plan.install_strategy, InstallStrategy::CopyPrebuiltSystem);
        assert!(plan
            .install_strategy_summary
            .contains("copies a prebuilt system closure"));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::NixosInstall
                && command
                    .argv
                    .iter()
                    .any(|arg| arg.contains("install mount") && !arg.contains("install nixos"))
        }));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::CopySystem
                && command.argv.iter().any(|arg| {
                    arg.contains("nix copy --no-check-sigs --to")
                        && arg.contains("--no-check-sigs")
                        && arg.contains("ssh-ng://root@192.0.2.10")
                        && arg.contains("remote-store=local%3Froot%3D%2Fmnt")
                        && arg.contains("NIX_SSHOPTS=")
                })
        }));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::NixosInstall
                && command.argv.iter().any(|arg| {
                    arg.contains("nixos-install --root /mnt --system")
                        && arg.contains("--no-channel-copy")
                })
        }));
        assert!(!plan
            .commands
            .iter()
            .any(|command| command.argv.iter().any(|arg| arg.contains("install nixos"))));
    }

    #[test]
    fn host_key_is_staged_after_mount_and_before_install() {
        let temp = tempfile::tempdir().unwrap();
        let host_key = temp.path().join("ssh_host_ed25519_key");
        fs::write(&host_key, "private-key").unwrap();
        fs::write(append_pub_suffix(&host_key), "public-key").unwrap();
        let mut command = fixture_command(&temp);
        command.host_key = Some(host_key.clone());
        command.no_final_reboot = true;

        let plan = build_plan(&command).unwrap();

        assert_eq!(plan.host_key, Some(host_key.display().to_string()));
        assert_eq!(
            plan.host_key_public,
            Some(append_pub_suffix(&host_key).display().to_string())
        );
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.contains("deploy-time secrets")));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::NixosInstall
                && command.argv.iter().any(|arg| arg.contains("install mount"))
        }));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::StageHostIdentity
                && command
                    .argv
                    .iter()
                    .any(|arg| arg.contains("ssh_host_ed25519_key"))
                && command.argv.iter().any(|arg| arg.contains("chmod 0600"))
        }));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::StageHostIdentity
                && command.argv.iter().any(|arg| {
                    arg.contains("install -o root -g root -m 0600")
                        && arg.contains("/mnt/etc/ssh/ssh_host_ed25519_key")
                        && arg.contains("rm -rf /tmp/nixos-kexec/host-identity")
                })
        }));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::NixosInstall
                && command.argv.iter().any(|arg| {
                    arg.contains("nixos-install --root /mnt --flake")
                        && arg.contains("github:example/flake#host")
                })
        }));
    }

    #[test]
    fn host_key_public_requires_private_host_key() {
        let temp = tempfile::tempdir().unwrap();
        let public_key = temp.path().join("ssh_host_ed25519_key.pub");
        fs::write(&public_key, "public-key").unwrap();
        let mut command = fixture_command(&temp);
        command.host_key_public = Some(public_key);

        let error = build_plan(&command).unwrap_err().to_string();

        assert!(error.contains("--host-key-public requires --host-key"));
    }

    #[test]
    fn generated_host_key_runs_hook_and_cleans_up_local_secret() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = fixture_command(&temp);
        command.generate_host_key = true;
        command.identity_hook = Some("flake-os-add-age-recipient".to_string());
        command.identity_rollback_hook = Some("flake-os-add-age-recipient".to_string());
        command.no_final_reboot = true;

        let plan = build_plan(&command).unwrap();
        let script = render_script(&plan);

        assert!(plan.generate_host_key);
        assert_eq!(plan.identity_host, Some("host".to_string()));
        assert!(script.contains("mktemp -d \"${TMPDIR:-/tmp}/nixos-kexec-host-key."));
        assert!(script.contains("trap cleanup_nixos_kexec_host_key EXIT"));
        assert!(script.contains("ssh-keygen -q -t ed25519 -N ''"));
        assert!(script.contains("NIXOS_KEXEC_SSH_PUBLIC_KEY_FILE=$NIXOS_KEXEC_HOST_KEY_PUBLIC"));
        assert!(script.contains("NIXOS_KEXEC_IDENTITY_STATE_DIR=\"$NIXOS_KEXEC_HOST_KEY_DIR\""));
        assert!(script
            .contains("NIXOS_KEXEC_IDENTITY_STATE_DIR=\"${NIXOS_KEXEC_IDENTITY_STATE_DIR:-}\""));
        assert!(script.contains("NIXOS_KEXEC_AGE_RECIPIENT"));
        assert!(script.contains("NIXOS_KEXEC_IDENTITY_HOOK_EVENT=apply"));
        assert!(script.contains("flake-os-add-age-recipient"));
        assert!(script.contains("nixos_kexec_identity_rollback()"));
        assert!(script.contains("NIXOS_KEXEC_IDENTITY_HOOK_EVENT=rollback"));
        assert!(script.contains("NIXOS_KEXEC_IDENTITY_ROLLBACK_ARMED=0"));
        assert!(script.contains("cleanup_nixos_kexec_host_key"));
        assert!(script.contains("< \"$NIXOS_KEXEC_HOST_KEY\""));
        assert!(script.contains("< \"$NIXOS_KEXEC_HOST_KEY_PUBLIC\""));
        assert!(script.contains("stat -c"));
        assert!(script.contains("%a"));
        assert!(script.contains("/mnt/etc/ssh/ssh_host_ed25519_key"));
        assert!(script.contains("rm -rf -- \"$NIXOS_KEXEC_HOST_KEY_DIR\""));
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.contains("removes the local private key")));
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.contains("identity rollback hooks run")));
    }

    #[test]
    fn identity_rollback_disarms_before_final_reboot() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = fixture_command(&temp);
        command.generate_host_key = true;
        command.identity_hook = Some("bootstrap-machine-identity".to_string());
        command.identity_rollback_hook = Some("bootstrap-machine-identity".to_string());

        let plan = build_plan(&command).unwrap();
        let disarm_index = plan
            .commands
            .iter()
            .position(|command| {
                command.description == "disarm identity rollback hook after successful deployment"
            })
            .unwrap();
        let reboot_index = plan
            .commands
            .iter()
            .position(|command| command.phase == Phase::Reboot)
            .unwrap();

        assert!(disarm_index < reboot_index);
    }

    #[test]
    fn static_host_key_identity_hook_exports_state_dir() {
        let temp = tempfile::tempdir().unwrap();
        let host_key = temp.path().join("ssh_host_ed25519_key");
        fs::write(&host_key, "private-key").unwrap();
        fs::write(host_key.with_extension("pub"), "public-key").unwrap();
        let mut command = fixture_command(&temp);
        command.host_key = Some(host_key);
        command.identity_hook = Some("bootstrap-machine-identity".to_string());
        command.identity_rollback_hook = Some("bootstrap-machine-identity".to_string());

        let plan = build_plan(&command).unwrap();
        let script = render_script(&plan);

        assert!(script.contains(
            "NIXOS_KEXEC_IDENTITY_STATE_DIR=$(mktemp -d \"${TMPDIR:-/tmp}/nixos-kexec-identity."
        ));
        assert!(script
            .contains("NIXOS_KEXEC_IDENTITY_STATE_DIR=\"${NIXOS_KEXEC_IDENTITY_STATE_DIR:-}\""));
        assert!(script.contains("NIXOS_KEXEC_IDENTITY_HOOK_EVENT=rollback"));
    }

    #[test]
    fn generated_host_key_conflicts_with_static_host_key() {
        let temp = tempfile::tempdir().unwrap();
        let host_key = temp.path().join("ssh_host_ed25519_key");
        fs::write(&host_key, "private-key").unwrap();
        let mut command = fixture_command(&temp);
        command.host_key = Some(host_key);
        command.generate_host_key = true;

        let error = build_plan(&command).unwrap_err().to_string();

        assert!(error.contains("--host-key and --generate-host-key are mutually exclusive"));
    }

    #[test]
    fn identity_hook_requires_host_identity() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = fixture_command(&temp);
        command.identity_hook = Some("flake-os-add-age-recipient".to_string());

        let error = build_plan(&command).unwrap_err().to_string();

        assert!(error.contains("--identity-hook requires --host-key or --generate-host-key"));
    }

    #[test]
    fn identity_rollback_hook_requires_identity_hook() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = fixture_command(&temp);
        command.generate_host_key = true;
        command.identity_rollback_hook = Some("flake-os-add-age-recipient".to_string());

        let error = build_plan(&command).unwrap_err().to_string();

        assert!(error.contains("--identity-rollback-hook requires --identity-hook"));
    }

    #[test]
    fn identity_hook_rejects_prebuilt_system_ordering() {
        let temp = tempfile::tempdir().unwrap();
        let system = temp.path().join("system");
        fs::create_dir(&system).unwrap();
        let mut command = fixture_command(&temp);
        command.system = Some(system);
        command.generate_host_key = true;
        command.identity_hook = Some("flake-os-add-age-recipient".to_string());

        let error = build_plan(&command).unwrap_err().to_string();

        assert!(error.contains("already built --system closure"));
    }

    #[test]
    fn installer_plan_builds_reviewable_nix_command() {
        let temp = tempfile::tempdir().unwrap();
        let command = fixture_installer_command(&temp);

        let plan = build_installer_plan(&command).unwrap();

        assert_eq!(plan.authorized_key_count, 2);
        assert_eq!(plan.argv[0], "nix");
        assert!(plan.argv.iter().any(|arg| arg == "--impure"));
        assert!(plan
            .argv
            .iter()
            .any(|arg| arg == &temp.path().join("installer-result").display().to_string()));
        assert!(plan.expression.contains("examples/kexec-installer.nix"));
        assert!(plan.expression.contains("networkManagerProfiles = { };"));
        assert!(plan
            .expression
            .contains("github:hermetic-foundation/disk-nix"));
        assert!(plan
            .expression
            .contains("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITest"));
    }

    #[test]
    fn installer_plan_can_load_network_manager_profiles_json() {
        let temp = tempfile::tempdir().unwrap();
        let profiles = temp.path().join("profiles.json");
        fs::write(&profiles, r#"{"home":{"connection":{"id":"home"}}}"#).unwrap();
        let mut command = fixture_installer_command(&temp);
        command.network_manager_profiles_json = Some(profiles.clone());

        let plan = build_installer_plan(&command).unwrap();

        assert_eq!(
            plan.network_manager_profiles_json,
            Some(profiles.display().to_string())
        );
        assert!(plan
            .expression
            .contains("networkManagerProfiles = builtins.fromJSON"));
        assert!(plan.expression.contains(&profiles.display().to_string()));
    }

    #[test]
    fn installer_plan_requires_authorized_key() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = fixture_installer_command(&temp);
        command.authorized_key_files = Vec::new();
        command.authorized_keys = Vec::new();

        let error = build_installer_plan(&command).unwrap_err().to_string();

        assert!(error.contains("requires at least one --authorized-key-file or --authorized-key"));
    }

    #[test]
    fn nix_string_escapes_interpolation_and_quotes() {
        assert_eq!(nix_string(r#"a"b${c}\"#), r#""a\"b\${c}\\""#);
    }
}
