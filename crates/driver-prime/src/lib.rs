use std::path::Path;

use shared_types::{LaunchSpec, LaunchSpecError, PermissionProfile, SessionDefinition};

pub const WSL_DISTRO: &str = "Ubuntu";
pub const SYSTEMD_RUN: &str = "/usr/bin/systemd-run";
pub const SYSTEMCTL: &str = "/usr/bin/systemctl";
pub const PYTHON: &str = "/usr/bin/python3";
pub const SCOPE_UNIT_PREFIX: &str = "prim1-session-";

/// Immutable supervisor-owned process guard. It intentionally does not format
/// user input into source code: every payload field remains a separate argv
/// element after the fixed program text.
pub const PRIME_GUARD: &str = "import os,signal,subprocess,sys; d=int(sys.argv[1]); i=int(sys.argv[2]); w=sys.argv[3]; s=os.stat(w); sys.exit(126) if (s.st_dev,s.st_ino)!=(d,i) else None; c=subprocess.Popen(sys.argv[4:],cwd=w); stop=lambda s,f: (_ for _ in ()).throw(SystemExit(128+s)); [signal.signal(s,stop) for s in (signal.SIGHUP,signal.SIGTERM,signal.SIGINT)]; raise SystemExit(c.wait())";

pub fn launch_spec(
    definition: &SessionDefinition,
    wsl_executable: &str,
    prime_executable: &str,
    scope_unit: &str,
    expected_device: u64,
    expected_inode: u64,
) -> Result<LaunchSpec, LaunchSpecError> {
    validate_direct_windows_program(wsl_executable)?;
    validate_linux_absolute_path(prime_executable)?;
    validate_linux_absolute_path(&definition.working_dir)?;

    if definition.permission_profile != PermissionProfile::Normal {
        return Err(LaunchSpecError::UnsupportedPermissionProfile {
            driver: definition.driver,
            profile: definition.permission_profile,
        });
    }
    if !valid_scope_unit(scope_unit) {
        return Err(LaunchSpecError::ProgramNotQualified {
            program: scope_unit.to_owned(),
        });
    }

    let args = vec![
        "-d".into(),
        WSL_DISTRO.into(),
        "--cd".into(),
        definition.working_dir.clone(),
        "--exec".into(),
        SYSTEMD_RUN.into(),
        "--user".into(),
        format!("--unit={scope_unit}.service"),
        "--property=KillMode=control-group".into(),
        "--property=Type=exec".into(),
        "--property=TimeoutStopSec=2s".into(),
        format!("--working-directory={}", definition.working_dir),
        "--pty".into(),
        "--wait".into(),
        "--collect".into(),
        "--quiet".into(),
        PYTHON.into(),
        "-c".into(),
        PRIME_GUARD.into(),
        expected_device.to_string(),
        expected_inode.to_string(),
        definition.working_dir.clone(),
        prime_executable.into(),
        "--cwd".into(),
        definition.working_dir.clone(),
    ];

    let neutral_working_dir = Path::new(wsl_executable)
        .parent()
        .unwrap_or_else(|| Path::new(r"C:\Windows\System32"))
        .to_string_lossy()
        .into_owned();

    Ok(LaunchSpec {
        program: wsl_executable.to_owned(),
        args,
        working_dir: neutral_working_dir,
        env: vec![shared_types::EnvVar {
            key: "WSLENV".into(),
            value: String::new(),
        }],
        display_name: definition.label.clone(),
    })
}

fn validate_direct_windows_program(program: &str) -> Result<(), LaunchSpecError> {
    let path = Path::new(program);
    if program.trim().is_empty() || !path.is_absolute() {
        return Err(LaunchSpecError::ProgramNotQualified {
            program: program.to_owned(),
        });
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if file_name != "wsl.exe" {
        return Err(LaunchSpecError::ShellMediatedProgram {
            program: program.to_owned(),
        });
    }
    Ok(())
}

fn validate_linux_absolute_path(path: &str) -> Result<(), LaunchSpecError> {
    if path.trim() != path || !path.starts_with('/') || path.contains('\0') {
        return Err(LaunchSpecError::ProgramNotQualified {
            program: path.to_owned(),
        });
    }
    Ok(())
}

fn valid_scope_unit(unit: &str) -> bool {
    unit.strip_prefix(SCOPE_UNIT_PREFIX).is_some_and(|suffix| {
        suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_types::{DriverKind, SessionId};

    #[cfg(windows)]
    const WSL: &str = r"C:\Windows\System32\wsl.exe";
    #[cfg(not(windows))]
    const WSL: &str = "/opt/prim1-test/wsl.exe";
    const PRIME: &str = "/home/alice/.npm-global/bin/prime-agent";
    const CWD: &str = "/home/alice/work & (qa)";
    const UNIT: &str = "prim1-session-11111111111111111111111111111111";

    fn definition(profile: PermissionProfile) -> SessionDefinition {
        SessionDefinition {
            session_id: SessionId::nil(),
            alias: "session-00000000-0000-0000-0000-000000000000".into(),
            label: "Prime & calc.exe".into(),
            driver: DriverKind::Prime,
            working_dir: CWD.into(),
            permission_profile: profile,
        }
    }

    #[test]
    fn normal_launch_is_shell_free_and_exact() {
        let spec = launch_spec(
            &definition(PermissionProfile::Normal),
            WSL,
            PRIME,
            UNIT,
            7,
            9,
        )
        .unwrap();
        assert_eq!(spec.program, WSL);
        assert_eq!(
            spec.args,
            vec![
                "-d",
                "Ubuntu",
                "--cd",
                CWD,
                "--exec",
                "/usr/bin/systemd-run",
                "--user",
                "--unit=prim1-session-11111111111111111111111111111111.service",
                "--property=KillMode=control-group",
                "--property=Type=exec",
                "--property=TimeoutStopSec=2s",
                "--working-directory=/home/alice/work & (qa)",
                "--pty",
                "--wait",
                "--collect",
                "--quiet",
                "/usr/bin/python3",
                "-c",
                PRIME_GUARD,
                "7",
                "9",
                CWD,
                PRIME,
                "--cwd",
                CWD,
            ]
        );
        #[cfg(windows)]
        assert_eq!(spec.working_dir, r"C:\Windows\System32");
        #[cfg(not(windows))]
        assert_eq!(spec.working_dir, "/opt/prim1-test");
        assert_eq!(spec.env[0].key, "WSLENV");
        assert_eq!(spec.env[0].value, "");
        assert!(!spec.args.iter().any(|arg| arg.contains("calc.exe")));
        assert!(
            !spec
                .args
                .iter()
                .any(|arg| matches!(arg.as_str(), "cmd.exe" | "sh" | "bash"))
        );
    }

    #[test]
    fn unsafe_relative_and_unowned_scope_are_rejected() {
        assert!(matches!(
            launch_spec(
                &definition(PermissionProfile::Unsafe),
                WSL,
                PRIME,
                UNIT,
                7,
                9
            ),
            Err(LaunchSpecError::UnsupportedPermissionProfile { .. })
        ));
        let mut relative = definition(PermissionProfile::Normal);
        relative.working_dir = "relative".into();
        assert!(launch_spec(&relative, WSL, PRIME, UNIT, 7, 9).is_err());
        assert!(
            launch_spec(
                &definition(PermissionProfile::Normal),
                WSL,
                "prime-agent",
                UNIT,
                7,
                9
            )
            .is_err()
        );
        assert!(
            launch_spec(
                &definition(PermissionProfile::Normal),
                WSL,
                PRIME,
                "other-11111111111111111111111111111111",
                7,
                9
            )
            .is_err()
        );
    }
}
