use color_eyre::eyre::{Result, bail};
use std::fmt;

use crate::output::output;

/// Represents a structured commit message with title, body, and trailers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitMessage {
    title: String,
    body: String,
    trailers: Vec<(String, String)>, // Preserve order
}

impl CommitMessage {
    /// Create a new commit message.
    pub fn new(title: String, body: String) -> Self {
        Self {
            title,
            body,
            trailers: Vec::new(),
        }
    }

    /// Parse a commit message from a string.
    pub fn parse(msg: &str) -> Self {
        let msg = msg.trim();
        if msg.is_empty() {
            return Self::new(String::new(), String::new());
        }

        let lines: Vec<&str> = msg.lines().collect();

        // First line is the title
        let title = lines.first().map(|s| s.to_string()).unwrap_or_default();

        if lines.len() == 1 {
            return Self::new(title, String::new());
        }

        // Find where trailers start (last paragraph that looks like trailers)
        let trailer_start = Self::find_trailer_start(&lines);

        // Extract body (everything between title and trailers)
        let body_lines = &lines[1..trailer_start];
        let body = body_lines.join("\n").trim().to_string();

        // Parse trailers
        let mut trailers = Vec::new();
        if trailer_start < lines.len() {
            trailers = Self::parse_trailers(&lines[trailer_start..]);
        }

        // Handle backwards compatibility: look for old-style sections in body
        let (body, legacy_trailers) = Self::extract_legacy_sections(&body);

        // Merge legacy trailers with parsed trailers (parsed trailers take precedence)
        let trailer_keys: std::collections::HashSet<_> =
            trailers.iter().map(|(k, _)| k.to_lowercase()).collect();
        for (key, value) in legacy_trailers {
            if !trailer_keys.contains(&key.to_lowercase()) {
                trailers.push((key, value));
            }
        }

        Self {
            title,
            body,
            trailers,
        }
    }

    /// Find where trailers start in the message.
    /// Returns the index of the first line that's part of the trailer block.
    fn find_trailer_start(lines: &[&str]) -> usize {
        if lines.len() <= 1 {
            return lines.len();
        }

        // Trailers must be in the last paragraph, separated by blank lines
        // Work backwards to find the last paragraph
        let mut last_para_end = lines.len();
        let mut last_para_start = lines.len();
        let mut in_content = false;

        for (i, line) in lines.iter().enumerate().rev() {
            if line.trim().is_empty() {
                if in_content {
                    // Found blank line before content, this ends the last paragraph search
                    last_para_start = i + 1;
                    break;
                }
            } else {
                if !in_content {
                    // Found first non-blank line from the end
                    last_para_end = i + 1;
                }
                in_content = true;
            }
        }

        // If we never found a blank line, the whole message is one paragraph
        if last_para_start >= last_para_end {
            last_para_start = 1; // Skip title line
        }

        // Check if the last paragraph looks like trailers
        if last_para_start >= lines.len() {
            return lines.len();
        }

        let para_lines = &lines[last_para_start..last_para_end];
        let trailer_like_count = para_lines
            .iter()
            .filter(|line| Self::is_trailer_line(line))
            .count();

        // If most lines look like trailers, treat it as a trailer block
        if trailer_like_count > 0 && trailer_like_count * 2 >= para_lines.len()
        {
            last_para_start
        } else {
            lines.len()
        }
    }

    /// Check if a line looks like a trailer.
    fn is_trailer_line(line: &str) -> bool {
        lazy_regex::regex!(r"^[A-Za-z0-9][\w-]*\s*:\s*.+$").is_match(line)
    }

    /// Parse trailer lines into key-value pairs.
    fn parse_trailers(lines: &[&str]) -> Vec<(String, String)> {
        let mut trailers = Vec::new();
        let regex = lazy_regex::regex!(r"^([A-Za-z0-9][\w-]*)\s*:\s*(.*)$");

        for line in lines {
            if let Some(caps) = regex.captures(line) {
                let key = caps.get(1).unwrap().as_str().to_string();
                let value = caps.get(2).unwrap().as_str().trim().to_string();
                trailers.push((key, value));
            }
        }

        trailers
    }

    /// Extract legacy-style sections from body for backwards compatibility.
    /// Returns (cleaned_body, legacy_trailers).
    fn extract_legacy_sections(body: &str) -> (String, Vec<(String, String)>) {
        let mut trailers = Vec::new();
        let mut cleaned_lines = Vec::new();
        let mut in_legacy_section = false;

        for line in body.lines() {
            // Check for old-style sections: "Pull Request:", "Reviewers:", "Reviewed By:"
            if let Some((key, value)) = Self::parse_legacy_section_line(line) {
                in_legacy_section = true;
                let trailer_key = match key.as_str() {
                    "Pull Request" => "Pull-request".to_string(),
                    _ => key,
                };
                trailers.push((trailer_key, value));
            } else if line.trim().is_empty() {
                if !in_legacy_section {
                    cleaned_lines.push(line);
                }
            } else {
                // If we hit a non-empty, non-section line after sections started,
                // it's not a clean section block, so include everything
                if in_legacy_section {
                    // This shouldn't happen in well-formed messages
                    in_legacy_section = false;
                }
                cleaned_lines.push(line);
            }
        }

        (cleaned_lines.join("\n").trim().to_string(), trailers)
    }

    /// Parse a legacy section line like "Pull Request: url" or "Reviewers: names".
    fn parse_legacy_section_line(line: &str) -> Option<(String, String)> {
        let regex = lazy_regex::regex!(
            r"^(Pull Request|Reviewers|Reviewed By)\s*:\s*(.*)$"
        );
        regex.captures(line).map(|caps| {
            (
                caps.get(1).unwrap().as_str().to_string(),
                caps.get(2).unwrap().as_str().trim().to_string(),
            )
        })
    }

    /// Get the title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Get the body.
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Get a trailer value by key (case-insensitive lookup).
    pub fn get_trailer(&self, key: &str) -> Option<&str> {
        let key_lower = key.to_lowercase();
        self.trailers
            .iter()
            .find(|(k, _)| k.to_lowercase() == key_lower)
            .map(|(_, v)| v.as_str())
    }

    /// Set a trailer value. If the key exists, updates it; otherwise adds it.
    pub fn set_trailer(&mut self, key: String, value: String) {
        let key_lower = key.to_lowercase();

        if let Some(pos) = self
            .trailers
            .iter()
            .position(|(k, _)| k.to_lowercase() == key_lower)
        {
            self.trailers[pos] = (key, value);
        } else {
            self.trailers.push((key, value));
        }
    }

    /// Remove a trailer by key (case-insensitive).
    pub fn remove_trailer(&mut self, key: &str) {
        let key_lower = key.to_lowercase();
        self.trailers.retain(|(k, _)| k.to_lowercase() != key_lower);
    }

    /// Get all trailers as a slice.
    pub fn trailers(&self) -> &[(String, String)] {
        &self.trailers
    }

    /// Set the title.
    pub fn set_title(&mut self, title: String) {
        self.title = title;
    }

    /// Set the body.
    pub fn set_body(&mut self, body: String) {
        self.body = body;
    }

    /// Validate the commit message.
    pub fn validate(&self, _config: &crate::config::Config) -> Result<()> {
        if self.title.is_empty() {
            output("💔", "Commit message does not have a title!")?;
            bail!("Commit message does not have a title!");
        }
        Ok(())
    }

    /// Build a GitHub PR body from this message.
    pub fn to_github_body(&self) -> String {
        self.body.clone()
    }

    /// Build a GitHub PR body for merging (includes trailers).
    pub fn to_github_body_for_merging(&self) -> String {
        let mut result = self.body.clone();

        // Add relevant trailers
        for (key, value) in &self.trailers {
            let key_lower = key.to_lowercase();
            if key_lower == "reviewers"
                || key_lower == "reviewed-by"
                || key_lower == "pull-request"
            {
                if !result.is_empty() {
                    result.push_str("\n\n");
                }
                result.push_str(key);
                result.push_str(": ");
                result.push_str(value);
            }
        }

        result
    }
}

impl fmt::Display for CommitMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Title
        write!(f, "{}", self.title)?;

        // Body (if present)
        if !self.body.is_empty() {
            write!(f, "\n\n{}", self.body)?;
        }

        // Trailers (if present)
        if !self.trailers.is_empty() {
            write!(f, "\n\n")?;
            for (key, value) in &self.trailers {
                writeln!(f, "{}: {}", key, value)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty() {
        let msg = CommitMessage::parse("");
        assert_eq!(msg.title(), "");
        assert_eq!(msg.body(), "");
        assert_eq!(msg.trailers().len(), 0);
    }

    #[test]
    fn test_parse_title_only() {
        let msg = CommitMessage::parse("Hello");
        assert_eq!(msg.title(), "Hello");
        assert_eq!(msg.body(), "");
        assert_eq!(msg.trailers().len(), 0);
    }

    #[test]
    fn test_parse_title_and_body() {
        let msg = CommitMessage::parse("Hello\n\nThis is the body");
        assert_eq!(msg.title(), "Hello");
        assert_eq!(msg.body(), "This is the body");
        assert_eq!(msg.trailers().len(), 0);
    }

    #[test]
    fn test_parse_with_trailers() {
        let msg = CommitMessage::parse(
            "Fix bug\n\nThis fixes the issue\n\nSigned-off-by: Alice <alice@example.com>\nReviewed-by: Bob",
        );
        assert_eq!(msg.title(), "Fix bug");
        assert_eq!(msg.body(), "This fixes the issue");
        assert_eq!(msg.trailers().len(), 2);
        assert_eq!(
            msg.get_trailer("Signed-off-by"),
            Some("Alice <alice@example.com>")
        );
        assert_eq!(msg.get_trailer("Reviewed-by"), Some("Bob"));
    }

    #[test]
    fn test_parse_legacy_pull_request() {
        let msg = CommitMessage::parse(
            "Title\n\nBody text\n\nPull Request: https://github.com/owner/repo/pull/123",
        );
        assert_eq!(msg.title(), "Title");
        assert_eq!(msg.body(), "Body text");
        assert_eq!(
            msg.get_trailer("Pull-request"),
            Some("https://github.com/owner/repo/pull/123")
        );
    }

    #[test]
    fn test_set_trailer() {
        let mut msg =
            CommitMessage::new("Title".to_string(), "Body".to_string());
        msg.set_trailer(
            "Pull-request".to_string(),
            "https://github.com/owner/repo/pull/123".to_string(),
        );
        assert_eq!(
            msg.get_trailer("Pull-request"),
            Some("https://github.com/owner/repo/pull/123")
        );
        assert_eq!(
            msg.get_trailer("pull-request"),
            Some("https://github.com/owner/repo/pull/123")
        );
    }

    #[test]
    fn test_remove_trailer() {
        let mut msg = CommitMessage::parse(
            "Title\n\nBody\n\nPull-request: url\nReviewers: alice",
        );
        assert_eq!(msg.trailers().len(), 2);
        msg.remove_trailer("Pull-request");
        assert_eq!(msg.trailers().len(), 1);
        assert_eq!(msg.get_trailer("Pull-request"), None);
        assert_eq!(msg.get_trailer("Reviewers"), Some("alice"));
    }

    #[test]
    fn test_to_string() {
        let mut msg =
            CommitMessage::new("Title".to_string(), "Body text".to_string());
        msg.set_trailer("Signed-off-by".to_string(), "Alice".to_string());
        let result = msg.to_string();
        assert_eq!(result, "Title\n\nBody text\n\nSigned-off-by: Alice\n");
    }

    #[test]
    fn test_case_insensitive_trailer_lookup() {
        let msg = CommitMessage::parse("Title\n\nBody\n\nPull-request: url");
        assert_eq!(msg.get_trailer("Pull-request"), Some("url"));
        assert_eq!(msg.get_trailer("pull-request"), Some("url"));
        assert_eq!(msg.get_trailer("PULL-REQUEST"), Some("url"));
    }
}
