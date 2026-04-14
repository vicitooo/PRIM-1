use shared_types::{DriverKind, LaunchSpec, SessionDefinition};

pub fn default_session(working_dir: &str) -> SessionDefinition {
    SessionDefinition {
        name: "terminal".into(),
        title: "Terminal".into(),
        driver: DriverKind::GenericTerminal,
        working_dir: working_dir.into(),
        command: None,
        args: vec![],
        env: vec![],
        auto_start: false,
    }
}

pub fn launch_spec(definition: &SessionDefinition) -> LaunchSpec {
    let (default_program, mut args) = if cfg!(windows) {
        ("powershell".to_string(), vec!["-NoLogo".into()])
    } else {
        ("bash".to_string(), vec!["-l".into()])
    };

    args.extend(definition.args.clone());

    LaunchSpec {
        program: definition.command.clone().unwrap_or(default_program),
        args,
        working_dir: definition.working_dir.clone(),
        env: definition.env.clone(),
        display_name: definition.title.clone(),
    }
}
