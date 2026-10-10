//! Output of what the tests do, as they do it

use std::io::Write as _;

/// Print a step of a test, indented under the test's name
pub fn step(text: impl AsRef<str>) {
    for line in text.as_ref().lines() {
        println!("     {line}");
    }
    let _ = std::io::stdout().flush();
}

/// The short form of an object ID, for output
pub fn short(id: &str) -> &str {
    &id[..id.len().min(7)]
}

/// The first line of a commit message
pub fn title(message: &str) -> &str {
    message.lines().next().unwrap_or_default()
}

/// A command line, for output: arguments with spaces are quoted
pub fn command_line(program: &str, args: &[&str]) -> String {
    let mut line = program.to_string();
    for arg in args {
        line.push(' ');
        if arg.contains(' ') || arg.is_empty() {
            line.push_str(&format!("'{arg}'"));
        } else {
            line.push_str(arg);
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_line() {
        assert_eq!(
            command_line("spr", &["diff", "-m", "Second version"]),
            "spr diff -m 'Second version'"
        );
        assert_eq!(command_line("spr", &["land"]), "spr land");
    }
}
