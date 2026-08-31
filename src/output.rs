use crate::app::ExitAction;

pub fn format_action(action: &ExitAction) -> String {
    match action {
        ExitAction::Edit {
            path,
            working_directory,
        } => format!(
            "(cd {} && $EDITOR {})",
            quote(working_directory.to_string_lossy().as_ref()),
            quote(path.to_string_lossy().as_ref())
        ),
        ExitAction::ChangeDirectory(path) => {
            format!("cd {}", quote(path.to_string_lossy().as_ref()))
        }
    }
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::app::ExitAction;

    use super::{format_action, quote};

    #[test]
    fn shell_quotes_single_quotes() {
        assert_eq!(quote("a b'c"), "'a b'\\''c'");
    }

    #[test]
    fn editor_runs_from_the_project_root_in_a_subshell() {
        let action = ExitAction::Edit {
            path: PathBuf::from("/project/src/main.rs"),
            working_directory: PathBuf::from("/project"),
        };
        assert_eq!(
            format_action(&action),
            "(cd '/project' && $EDITOR '/project/src/main.rs')"
        );
    }
}
