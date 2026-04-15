use shared_types::{DriverKind, LaunchSpec, SessionDefinition};

pub fn default_session(working_dir: &str) -> SessionDefinition {
    SessionDefinition {
        name: "claude".into(),
        title: "Claude".into(),
        driver: DriverKind::Claude,
        working_dir: working_dir.into(),
        command: None,
        args: vec![],
        env: vec![],
        auto_start: false,
    }
}

pub fn launch_spec(definition: &SessionDefinition) -> LaunchSpec {
    let mut args = vec![
        "-n".into(),
        definition.name.clone(),
        "--dangerously-skip-permissions".into(),
        "--add-dir".into(),
        definition.working_dir.clone(),
    ];
    args.extend(definition.args.clone());

    LaunchSpec {
        program: definition
            .command
            .clone()
            .unwrap_or_else(|| "claude".into()),
        args,
        working_dir: definition.working_dir.clone(),
        env: definition.env.clone(),
        display_name: definition.title.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_spec_includes_named_session_and_working_dir() {
        let definition = default_session(r"D:\workspace");
        let spec = launch_spec(&definition);

        assert_eq!(spec.program, "claude");
        assert_eq!(
            spec.args,
            vec![
                "-n".to_string(),
                "claude".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--add-dir".to_string(),
                r"D:\workspace".to_string(),
            ]
        );
        assert_eq!(spec.working_dir, r"D:\workspace");
        assert_eq!(spec.display_name, "Claude");
    }
}
