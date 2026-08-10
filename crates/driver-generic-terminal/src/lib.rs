use std::path::Path;

use shared_types::{DriverKind, LaunchSpec, LaunchSpecError, PermissionProfile, SessionDefinition};

pub fn launch_spec(
    definition: &SessionDefinition,
    executable: &str,
) -> Result<LaunchSpec, LaunchSpecError> {
    validate_direct_program(executable)?;
    if definition.permission_profile != PermissionProfile::Normal {
        return Err(LaunchSpecError::UnsupportedPermissionProfile {
            driver: DriverKind::GenericTerminal,
            profile: definition.permission_profile,
        });
    }

    let args = if cfg!(windows) {
        vec!["-NoLogo".into()]
    } else {
        vec!["-l".into()]
    };

    Ok(LaunchSpec {
        program: executable.to_string(),
        args,
        working_dir: definition.working_dir.clone(),
        env: Vec::new(),
        display_name: definition.label.clone(),
    })
}

fn validate_direct_program(program: &str) -> Result<(), LaunchSpecError> {
    let path = Path::new(program);
    if program.trim().is_empty() || !path.is_absolute() {
        return Err(LaunchSpecError::ProgramNotQualified {
            program: program.to_string(),
        });
    }

    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(extension.as_str(), "cmd" | "bat" | "ps1") {
        return Err(LaunchSpecError::ShellMediatedProgram {
            program: program.to_string(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_types::SessionId;

    #[cfg(windows)]
    const WORKSPACE_ROOT: &str = r"C:\workspace & (qa)";
    #[cfg(windows)]
    const TERMINAL_EXECUTABLE: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    #[cfg(windows)]
    const SCRIPT_SHIM: &str = r"C:\workspace\terminal.ps1";

    #[cfg(not(windows))]
    const WORKSPACE_ROOT: &str = "/workspace & (qa)";
    #[cfg(not(windows))]
    const TERMINAL_EXECUTABLE: &str = "/bin/bash";
    #[cfg(not(windows))]
    const SCRIPT_SHIM: &str = "/tmp/terminal.ps1";

    fn definition(permission_profile: PermissionProfile) -> SessionDefinition {
        SessionDefinition {
            session_id: SessionId::nil(),
            alias: "session-00000000-0000-0000-0000-000000000000".into(),
            label: "Terminal & calc.exe".into(),
            driver: DriverKind::GenericTerminal,
            working_dir: WORKSPACE_ROOT.into(),
            permission_profile,
        }
    }

    #[test]
    fn normal_terminal_launches_the_qualified_program_directly() {
        let definition = definition(PermissionProfile::Normal);
        let spec = launch_spec(&definition, TERMINAL_EXECUTABLE).unwrap();

        assert_eq!(spec.program, TERMINAL_EXECUTABLE);
        if cfg!(windows) {
            assert_eq!(spec.args, vec!["-NoLogo".to_string()]);
        } else {
            assert_eq!(spec.args, vec!["-l".to_string()]);
        }
        assert_eq!(spec.working_dir, WORKSPACE_ROOT);
        assert_eq!(spec.display_name, definition.label);
        assert!(spec.env.is_empty());
        assert!(!spec.args.iter().any(|arg| arg.contains("calc.exe")));
    }

    #[test]
    fn generic_terminal_rejects_unsafe_permission_profile() {
        let error =
            launch_spec(&definition(PermissionProfile::Unsafe), TERMINAL_EXECUTABLE).unwrap_err();
        assert_eq!(
            error,
            LaunchSpecError::UnsupportedPermissionProfile {
                driver: DriverKind::GenericTerminal,
                profile: PermissionProfile::Unsafe,
            }
        );
    }

    #[test]
    fn relative_and_script_programs_are_rejected() {
        let definition = definition(PermissionProfile::Normal);
        assert!(matches!(
            launch_spec(&definition, "powershell"),
            Err(LaunchSpecError::ProgramNotQualified { .. })
        ));
        assert!(matches!(
            launch_spec(&definition, SCRIPT_SHIM),
            Err(LaunchSpecError::ShellMediatedProgram { .. })
        ));
    }
}
