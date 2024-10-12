/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::error::{Error, Result};

use std::{future::Future, io::Write, process::Stdio, time::Duration};
use unicode_normalization::UnicodeNormalization;

pub fn slugify(s: &str) -> String {
    s.trim()
        .nfd()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || c == &'_' || c == &'-')
        .map(|c| char::to_ascii_lowercase(&c))
        .scan(None, |last_char, char| {
            if char == '-' && last_char == &Some('-') {
                Some(None)
            } else {
                *last_char = Some(char);
                Some(Some(char))
            }
        })
        .flatten()
        .collect()
}

pub fn parse_name_list(text: &str) -> Vec<String> {
    lazy_regex::regex!(r#"\(.*?\)"#)
        .replace_all(text, ",")
        .split(',')
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .map(String::from)
        .collect()
}

pub fn remove_all_parens(text: &str) -> String {
    lazy_regex::regex!(r#"[()]"#).replace_all(text, "").into()
}

pub async fn run_command(cmd: &mut tokio::process::Command) -> Result<()> {
    let cmd_output = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?
        .wait_with_output()
        .await?;

    if !cmd_output.status.success() {
        console::Term::stderr().write_all(&cmd_output.stderr)?;
        return Err(Error::new("command failed"));
    }

    Ok(())
}

pub async fn do_with_retry<F, Fut, FOut, H>(
    f: F,
    attempts: u64,
    on_error: H,
    sleep_time: Duration,
) -> Result<FOut>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<FOut>>,
    H: Fn(&Error) -> Result<()>,
{
    let mut last_error = None;
    for _ in 0..attempts {
        match f().await {
            Ok(val) => return Ok(val),
            Err(err) => {
                on_error(&err)?;
                tokio::time::sleep(sleep_time).await;
                last_error = Some(err);
            }
        }
    }
    Err(last_error.unwrap())
}

#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;

    #[test]
    fn test_empty() {
        assert_eq!(slugify(""), "".to_string());
    }

    #[test]
    fn test_hello_world() {
        assert_eq!(slugify(" Hello  World! "), "hello-world".to_string());
    }

    #[test]
    fn test_accents() {
        assert_eq!(slugify("ĥêlļō ŵöřľď"), "hello-world".to_string());
    }

    #[test]
    fn test_parse_name_list_empty() {
        assert!(parse_name_list("").is_empty());
        assert!(parse_name_list(" ").is_empty());
        assert!(parse_name_list("  ").is_empty());
        assert!(parse_name_list("   ").is_empty());
        assert!(parse_name_list("\n").is_empty());
        assert!(parse_name_list(" \n ").is_empty());
    }

    #[test]
    fn test_parse_name_single_name() {
        assert_eq!(parse_name_list("foo"), vec!["foo".to_string()]);
        assert_eq!(parse_name_list("foo  "), vec!["foo".to_string()]);
        assert_eq!(parse_name_list("  foo"), vec!["foo".to_string()]);
        assert_eq!(parse_name_list("  foo  "), vec!["foo".to_string()]);
        assert_eq!(parse_name_list("foo (Foo Bar)"), vec!["foo".to_string()]);
        assert_eq!(
            parse_name_list("  foo (Foo Bar)  "),
            vec!["foo".to_string()]
        );
        assert_eq!(
            parse_name_list(" () (-)foo (Foo Bar)  (xx)"),
            vec!["foo".to_string()]
        );
    }

    #[test]
    fn test_parse_name_multiple_names() {
        let expected =
            vec!["foo".to_string(), "bar".to_string(), "baz".to_string()];
        assert_eq!(parse_name_list("foo,bar,baz"), expected);
        assert_eq!(parse_name_list("foo, bar, baz"), expected);
        assert_eq!(parse_name_list("foo , bar , baz"), expected);
        assert_eq!(
            parse_name_list("foo (Mr Foo), bar (Ms Bar), baz (Dr Baz)"),
            expected
        );
        assert_eq!(
            parse_name_list(
                "foo (Mr Foo) bar (Ms Bar) (the other one), baz (Dr Baz)"
            ),
            expected
        );
    }
}
