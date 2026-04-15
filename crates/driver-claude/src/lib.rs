use std::path::Path;

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
    let wrapper_root = wrapper_root_for_session(&definition.working_dir);
    let mut args = vec![
        "-n".into(),
        definition.name.clone(),
        "--dangerously-skip-permissions".into(),
        "--add-dir".into(),
        definition.working_dir.clone(),
        "--add-dir".into(),
        wrapper_root,
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

fn wrapper_root_for_session(working_dir: &str) -> String {
    let path = Path::new(working_dir);
    let is_wrapper_root = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.eq_ignore_ascii_case("CLI-master-wrapper"))
        .unwrap_or(false);

    if is_wrapper_root {
        working_dir.to_string()
    } else {
        path.join("CLI-master-wrapper").to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_spec_includes_named_session_and_allowed_dirs() {
        let definition = default_session(r"<workspace>");
        let spec = launch_spec(&definition);

        assert_eq!(spec.program, "claude");
        assert_eq!(
            spec.args,
            vec![
                "-n".to_string(),
                "claude".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--add-dir".to_string(),
                r"<workspace>".to_string(),
                "--add-dir".to_string(),
                r".".to_string(),
            ]
        );
        assert_eq!(spec.working_dir, r"<workspace>");
        assert_eq!(spec.display_name, "Claude");
    }

    #[test]
    fn wrapper_root_helper_keeps_existing_wrapper_path() {
        assert_eq!(
            wrapper_root_for_session(r"."),
            r"."
        );
    }
}
