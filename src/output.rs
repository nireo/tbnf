use crate::app::ExitAction;

pub fn format_action(action: &ExitAction) -> String {
    match action {
        ExitAction::Edit(path) => format!("$EDITOR {}", quote(path.to_string_lossy().as_ref())),
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
    use super::quote;

    #[test]
    fn shell_quotes_single_quotes() {
        assert_eq!(quote("a b'c"), "'a b'\\''c'");
    }
}
