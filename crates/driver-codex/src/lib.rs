use shared_types::{DriverKind, LaunchSpec, SessionDefinition};

pub fn default_session(working_dir: &str) -> SessionDefinition {
    SessionDefinition {
        name: "codex".into(),
        title: "Codex".into(),
        driver: DriverKind::Codex,
        working_dir: working_dir.into(),
        command: None,
        args: vec![],
        env: vec![],
        auto_start: false,
    }
}

pub fn launch_spec(definition: &SessionDefinition) -> LaunchSpec {
    let (program, mut args) = if cfg!(windows) {
        (
            "cmd.exe".to_string(),
            vec![
                "/d".into(),
                "/c".into(),
                "codex.cmd".into(),
                "--yolo".into(),
                "--no-alt-screen".into(),
                "-C".into(),
                definition.working_dir.clone(),
            ],
        )
    } else {
        (
            definition.command.clone().unwrap_or_else(|| "codex".into()),
            vec![
                "--yolo".into(),
                "--no-alt-screen".into(),
                "-C".into(),
                definition.working_dir.clone(),
            ],
        )
    };
    args.extend(definition.args.clone());

    LaunchSpec {
        program,
        args,
        working_dir: definition.working_dir.clone(),
        env: definition.env.clone(),
        display_name: definition.title.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_launch_spec_uses_cmd_shim_and_cwd_flag() {
        let definition = default_session(r"D:\workspace");
        let spec = launch_spec(&definition);

        assert_eq!(spec.program, "cmd.exe");
        assert_eq!(
            spec.args,
            vec![
                "/d".to_string(),
                "/c".to_string(),
                "codex.cmd".to_string(),
                "--yolo".to_string(),
                "--no-alt-screen".to_string(),
                "-C".to_string(),
                r"D:\workspace".to_string(),
            ]
        );
        assert_eq!(spec.working_dir, r"D:\workspace");
        assert_eq!(spec.display_name, "Codex");
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_launch_spec_uses_codex_binary_and_cwd_flag() {
        let definition = default_session("/workspace");
        let spec = launch_spec(&definition);

        assert_eq!(spec.program, "codex");
        assert_eq!(
            spec.args,
            vec![
                "--yolo".to_string(),
                "--no-alt-screen".to_string(),
                "-C".to_string(),
                "/workspace".to_string(),
            ]
        );
        assert_eq!(spec.working_dir, "/workspace");
        assert_eq!(spec.display_name, "Codex");
    }
}
