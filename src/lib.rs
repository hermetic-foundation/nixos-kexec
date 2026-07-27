use std::{
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use serde::Serialize;
use thiserror::Error;

const DEFAULT_TARGET_ROOT: &str = "/mnt";
const DEFAULT_REMOTE_WORKDIR: &str = "/tmp/nixos-kexec";
const DEFAULT_DISK_NIX: &str = "github:hermetic-foundation/disk-nix#disk-nix";

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
    NixosInstall,
    Reboot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentPlan {
    pub target: String,
    pub flake: String,
    pub install_flake: String,
    pub flake_source: Option<String>,
    pub system: Option<String>,
    pub disk_spec: String,
    pub disk_nix: String,
    pub disk_nix_command: Option<String>,
    pub ssh_tty: bool,
    pub target_root: String,
    pub remote_workdir: String,
    pub commands: Vec<PlanCommand>,
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
        CommandKind::Completions { shell } => {
            let mut command = Cli::command();
            let name = command.get_name().to_string();
            generate(shell, &mut command, name, output);
        }
    }
    Ok(())
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
    if let Some(system) = &command.system {
        require_existing_path("system closure", system)?;
    }

    let remote_spec = format!("{}/disk-nix-install.json", command.remote_workdir);
    let remote_kernel = format!("{}/kexec/kernel", command.remote_workdir);
    let remote_initrd = format!("{}/kexec/initrd", command.remote_workdir);
    let remote_flake_source = format!("{}/flake-source", command.remote_workdir);
    let kexec_append = effective_kexec_append(command)?;
    let install_flake = install_flake(command, &remote_flake_source)?;
    let mut commands = Vec::new();

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
            "set -euo pipefail; kexec -l {} --initrd={} --append {}; echo 'nixos-kexec: entering kexec' >&2; sync; systemctl kexec || kexec -e",
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
        commands.push(copy_system_command(command, system));
        commands.push(ssh_command(
            command,
            Phase::NixosInstall,
            "install prebuilt NixOS system closure",
            true,
            format!(
                "set -euo pipefail; nixos-install --root {} --system {} --no-root-passwd",
                shell_quote(&command.target_root),
                shell_quote(&system.display().to_string())
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
    if !command.no_final_reboot {
        commands.push(ssh_command(
            command,
            Phase::Reboot,
            "sync and reboot into the installed system",
            true,
            "set -euo pipefail; sync; reboot".to_string(),
        ));
    }

    Ok(DeploymentPlan {
        target: command.target.clone(),
        flake: command.flake.clone(),
        install_flake,
        flake_source: command
            .flake_source
            .as_ref()
            .map(|path| path.display().to_string()),
        system: command.system.as_ref().map(|path| path.display().to_string()),
        disk_spec: command.disk_spec.display().to_string(),
        disk_nix: command.disk_nix.clone(),
        disk_nix_command: command.disk_nix_command.clone(),
        ssh_tty: command.ssh_tty,
        target_root: command.target_root.clone(),
        remote_workdir: command.remote_workdir.clone(),
        commands,
        warnings: vec![
            "kexec replaces the running kernel immediately; keep console or power access available".to_string(),
            "disk-nix apply and nixos-install are destructive when the install spec formats disks".to_string(),
            "the kexec installer environment must boot with SSH, Nix, kexec-tools, and network access".to_string(),
        ],
    })
}

pub fn render_script(plan: &DeploymentPlan) -> String {
    let mut script = String::from(
        "#!/usr/bin/env bash\nset -euo pipefail\n\n# Generated by nixos-kexec. Review before running.\n\n",
    );
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
        script.push_str(&shell_command(&command.argv));
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
    if let Some(source) = &plan.flake_source {
        writeln!(output, "flake source: {source}")?;
    }
    if let Some(system) = &plan.system {
        writeln!(output, "system: {system}")?;
    }
    writeln!(output, "disk spec: {}", plan.disk_spec)?;
    writeln!(output, "disk-nix: {}", plan.disk_nix)?;
    if let Some(command) = &plan.disk_nix_command {
        writeln!(output, "disk-nix command: {command}")?;
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
        writeln!(output, "  {}", shell_command(&command.argv))?;
    }
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
        &std::iter::once("ssh".to_string())
            .chain(command.ssh_tty.then(|| "-tt".to_string()))
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
                "output=$({ssh} 2>&1); status=$?; printf '%s\\n' \"$output\"; [ \"$status\" -eq 0 ] || {{ [ \"$status\" -eq 255 ] && case \"$output\" in *'nixos-kexec: entering kexec'*) true ;; *) false ;; esac; }}"
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
        "deadline=$((SECONDS + 600)); until {ssh}; do if [ \"$SECONDS\" -ge \"$deadline\" ]; then echo 'timed out waiting for installer SSH' >&2; exit 1; fi; sleep 5; done"
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
            system: None,
            disk_spec,
            kexec_kernel: kernel,
            kexec_initrd: initrd,
            kexec_append: Some("console=ttyS0".to_string()),
            disk_nix: DEFAULT_DISK_NIX.to_string(),
            disk_nix_command: None,
            ssh_tty: false,
            target_root: DEFAULT_TARGET_ROOT.to_string(),
            remote_workdir: DEFAULT_REMOTE_WORKDIR.to_string(),
            ssh_options: vec!["StrictHostKeyChecking=accept-new".to_string()],
            script_out: None,
            json: false,
            no_final_reboot: false,
        }
    }

    #[test]
    fn ssh_plan_contains_kexec_disk_nix_install_and_reboot_phases() {
        let temp = tempfile::tempdir().unwrap();
        let plan = build_plan(&fixture_command(&temp)).unwrap();

        assert_eq!(plan.commands.len(), 9);
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::Kexec
                && command.mutates
                && command.argv.iter().any(|arg| arg.contains("kexec -l"))
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
        assert!(script.contains("WARNING: kexec replaces the running kernel immediately"));
        assert!(script.contains("ssh -o StrictHostKeyChecking=accept-new root@192.0.2.10"));
        assert!(script.contains("until timeout 15s ssh"));
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
                    arg.contains("nix copy --to")
                        && arg.contains("ssh-ng://root@192.0.2.10")
                        && arg.contains("remote-store=local%3Froot%3D%2Fmnt")
                        && arg.contains("NIX_SSHOPTS=")
                })
        }));
        assert!(plan.commands.iter().any(|command| {
            command.phase == Phase::NixosInstall
                && command
                    .argv
                    .iter()
                    .any(|arg| arg.contains("nixos-install --root /mnt --system"))
        }));
        assert!(!plan
            .commands
            .iter()
            .any(|command| command.argv.iter().any(|arg| arg.contains("install nixos"))));
    }
}
