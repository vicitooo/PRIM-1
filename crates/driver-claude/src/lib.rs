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
