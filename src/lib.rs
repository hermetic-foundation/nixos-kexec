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
    #[arg(long, default_value = "console=tty0")]
    kexec_append: String,
    /// disk-nix flake app used inside the installer environment.
    #[arg(long, default_value = DEFAULT_DISK_NIX)]
    disk_nix: String,
    /// Mount target passed to disk-nix install nixos.
    #[arg(long, default_value = DEFAULT_TARGET_ROOT)]
    target_root: String,
    /// Remote temporary work directory.
    #[arg(long, default_value = DEFAULT_REMOTE_WORKDIR)]
    remote_workdir: String,
    /// Extra option passed to ssh and scp.
    #[arg(long = "ssh-option", value_name = "OPTION")]
    ssh_options: Vec<String>,
    /// Write the rendered script to this path.
    #[arg(long, value_name = "PATH")]
    script_out: Option<PathBuf>,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    json: bool,
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
    NixosInstall,
    Reboot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentPlan {
    pub target: String,
    pub flake: String,
    pub disk_spec: String,
    pub disk_nix: String,
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

    let remote_spec = format!("{}/disk-nix-install.json", command.remote_workdir);
    let remote_kernel = format!("{}/kexec/kernel", command.remote_workdir);
    let remote_initrd = format!("{}/kexec/initrd", command.remote_workdir);
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
    commands.push(scp_command(
        command,
        Phase::StageKexec,
        "upload kexec kernel and initrd to the current host",
        false,
        vec![
            command.kexec_kernel.display().to_string(),
            command.kexec_initrd.display().to_string(),
            format!("{}:{}/kexec/", command.target, command.remote_workdir),
        ],
    ));
    commands.push(ssh_command(
        command,
        Phase::Kexec,
        "load and enter the staged kexec installer",
        true,
        format!(
            "set -euo pipefail; kexec -l {} --initrd={} --append {}; sync; systemctl kexec || kexec -e",
            shell_quote(&remote_kernel),
            shell_quote(&remote_initrd),
            shell_quote(&command.kexec_append)
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
            "set -euo pipefail; mkdir -p {}",
            shell_quote(&command.remote_workdir)
        ),
    ));
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
            "set -euo pipefail; nix --extra-experimental-features 'nix-command flakes' run {} -- apply --spec {} --probe-current --execute",
            shell_quote(&command.disk_nix),
            shell_quote(&remote_spec)
        ),
    ));
    commands.push(ssh_command(
        command,
        Phase::NixosInstall,
        "mount target storage and run nixos-install through disk-nix",
        true,
        format!(
            "set -euo pipefail; nix --extra-experimental-features 'nix-command flakes' run {} -- install nixos --spec {} --flake {} --target {} --execute",
            shell_quote(&command.disk_nix),
            shell_quote(&remote_spec),
            shell_quote(&command.flake),
            shell_quote(&command.target_root)
        ),
    ));
    commands.push(ssh_command(
        command,
        Phase::Reboot,
        "sync and reboot into the installed system",
        true,
        "set -euo pipefail; sync; reboot".to_string(),
    ));

    Ok(DeploymentPlan {
        target: command.target.clone(),
        flake: command.flake.clone(),
        disk_spec: command.disk_spec.display().to_string(),
        disk_nix: command.disk_nix.clone(),
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
    writeln!(output, "disk spec: {}", plan.disk_spec)?;
    writeln!(output, "disk-nix: {}", plan.disk_nix)?;
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

fn ssh_command(
    command: &SshCommand,
    phase: Phase,
    description: &str,
    mutates: bool,
    remote_script: String,
) -> PlanCommand {
    let mut argv = vec!["ssh".to_string()];
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
        &std::iter::once("ssh".to_string())
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
            disk_spec,
            kexec_kernel: kernel,
            kexec_initrd: initrd,
            kexec_append: "console=ttyS0".to_string(),
            disk_nix: DEFAULT_DISK_NIX.to_string(),
            target_root: DEFAULT_TARGET_ROOT.to_string(),
            remote_workdir: DEFAULT_REMOTE_WORKDIR.to_string(),
            ssh_options: vec!["StrictHostKeyChecking=accept-new".to_string()],
            script_out: None,
            json: false,
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
}
