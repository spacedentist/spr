use crate::error::{Error, Result};
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

pub fn get_branch_name_from_ref_name(r: &str) -> Result<&str> {
    if r.starts_with("refs/") {
        if let Some(branch_name) = r.strip_prefix("refs/heads/") {
            Ok(branch_name)
        } else {
            Err(Error::new(format!("Ref '{r}' does not refer to a branch")))
        }
    } else {
        Ok(r)
    }
}

pub fn normalise_ref<'a, T: Into<std::borrow::Cow<'a, str>>>(
    r: T,
) -> std::borrow::Cow<'a, str> {
    let r: std::borrow::Cow<str> = r.into();

    if r.starts_with("refs/") {
        r
    } else {
        format!("refs/heads/{r}").into()
    }
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
