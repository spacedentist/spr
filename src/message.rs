//! spr's model of a commit message: a title, a body, and trailers at
//! the end, like `Signed-off-by:`. spr maintains some of the trailers
//! itself (`Pull-request`, `Reviewers`, `Reviewed-by`, and the
//! local-only `Spr-Id` and `Spr-Target`, see `is_spr_trailer`); all the
//! rest is the user's, and spr keeps it as it is.
//!
//! `CommitMessage::parse` also recognises the messages spr 1.x wrote,
//! whose fields were labeled sections at the end of the message rather
//! than trailers, and maps them to spr's trailers (see
//! `parse_legacy_message`).

use color_eyre::eyre::{Result, bail};
use std::fmt;

use crate::output::output;

/// The trailer with the ID spr gives a local commit (see `new_spr_id`)
pub const SPR_ID_TRAILER: &str = "Spr-Id";

/// The trailer with the target of a local commit's Pull Request, if it's
/// not the default target. Like `Spr-Id`, it only exists locally: it's
/// neither in the Pull Request's description nor in the landed commit.
pub const SPR_TARGET_TRAILER: &str = "Spr-Target";

/// The trailers spr maintains based on the Pull Request: its URL, the
/// requested reviewers and who approved it. On GitHub, that information is
/// part of the Pull Request itself, so these trailers aren't in its
/// description; the landed commit gets them from the Pull Request.
const PULL_REQUEST_TRAILERS: [&str; 3] =
    ["Pull-request", "Reviewers", "Reviewed-by"];

/// The trailers that only exist in local commits
const LOCAL_ONLY_TRAILERS: [&str; 2] = [SPR_ID_TRAILER, SPR_TARGET_TRAILER];

/// Whether spr manages the trailer with this key: it either reflects the
/// Pull Request, or only exists locally. All other trailers (e.g.
/// `Co-authored-by`, `Signed-off-by`) mean nothing to spr: they are the
/// user's content, like the rest of the commit message, so they go into the
/// Pull Request's description and from there into the landed commit.
fn is_spr_trailer(key: &str) -> bool {
    is_pull_request_trailer(key) || is_local_only_trailer(key)
}

fn is_pull_request_trailer(key: &str) -> bool {
    PULL_REQUEST_TRAILERS
        .iter()
        .any(|trailer| trailer.eq_ignore_ascii_case(key))
}

fn is_local_only_trailer(key: &str) -> bool {
    LOCAL_ONLY_TRAILERS
        .iter()
        .any(|trailer| trailer.eq_ignore_ascii_case(key))
}

/// A new random ID for a local commit: 128 bits, as 32 hex digits.
///
/// It identifies a unit of work (a local commit and its Pull Request)
/// across amends and rebases, like Gerrit's Change-Id. It only needs to be
/// unique, not unpredictable: the randomness comes from the standard
/// library's `RandomState`, which is seeded from the operating system.
pub fn new_spr_id() -> String {
    use std::hash::{BuildHasher, Hasher};

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();

    (0..2u8)
        .map(|part| {
            let mut hasher =
                std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u128(nanos);
            hasher.write_u32(std::process::id());
            hasher.write_u8(part);
            format!("{:016x}", hasher.finish())
        })
        .collect()
}

/// Whether the given string is an ID as created by `new_spr_id`: 32 hex
/// digits (lowercase). IDs come from commit messages, which users can edit,
/// and are used in ref names (`refs/spr/<id>/…`), so anything else must not
/// be used as an ID.
pub fn is_valid_spr_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

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

    /// The message of a Pull Request, from its title and description. The
    /// trailers at the end of the description (see `to_github_body`) become
    /// the message's trailers. Trailers that spr manages are ignored: the
    /// caller sets the ones that reflect the Pull Request from GitHub's data,
    /// and the local-only ones don't belong there.
    pub fn from_pull_request(title: String, description: &str) -> Self {
        // spr 1.x wrote its sections only into local commit messages,
        // never into a Pull Request's description, so a description is
        // never a message spr 1.x wrote: a line like "Pull Request: …"
        // in it is description text. (With the legacy parse it would
        // become a Pull-request trailer, which this function drops, so
        // `spr amend` and `spr land` would lose the line silently.)
        let mut message =
            Self::parse_impl(&format!("{title}\n\n{description}"), false);
        message.title = title;
        message.trailers.retain(|(key, _)| !is_spr_trailer(key));
        message
    }

    /// Parse a commit message from a string.
    pub fn parse(msg: &str) -> Self {
        Self::parse_impl(msg, true)
    }

    /// Parse a commit message from a string. `legacy_sections` decides
    /// whether messages written by spr 1.x are recognised by their
    /// sections at the end of the message (see `parse_legacy_message`).
    /// Only local commit messages need that: spr 1.x never wrote its
    /// sections into a Pull Request's description (see
    /// `from_pull_request`).
    fn parse_impl(msg: &str, legacy_sections: bool) -> Self {
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

        // Parse trailers and extract any non-trailer lines
        let mut trailers = Vec::new();
        let mut non_trailer_lines = Vec::new();
        if trailer_start < lines.len() {
            (trailers, non_trailer_lines) =
                Self::parse_trailers(&lines[trailer_start..]);
        }

        // spr 1.x, before it had trailers, wrote its fields as labeled
        // sections at the end of the message, and a "Pull Request:"
        // section only ever came from it. A message that ends in one is
        // parsed with spr 1.x's rules, so its fields are still
        // recognised - unless the trailer block has spr 2.0's own
        // Pull-request trailer, which means spr 2.0 wrote the message.
        // Everywhere else these labels are body text, not fields.
        if legacy_sections
            && !trailers
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case("Pull-request"))
            && let Some(mut message) =
                Self::parse_legacy_message(&title, &lines[..trailer_start])
        {
            if !non_trailer_lines.is_empty() {
                if !message.body.is_empty() {
                    message.body.push_str("\n\n");
                }
                message.body.push_str(&non_trailer_lines.join("\n"));
            }
            message.trailers.extend(trailers);
            return message;
        }

        // Extract body (everything between title and trailers)
        let mut body = lines[1..trailer_start].join("\n").trim().to_string();

        // Append non-trailer lines from the trailer block to the body
        if !non_trailer_lines.is_empty() {
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            body.push_str(&non_trailer_lines.join("\n"));
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

        // Count trailer lines, including continuation lines (leading
        // whitespace) that follow a trailer
        let mut trailer_like_count = 0;
        let mut in_trailer = false;
        for line in para_lines {
            if Self::is_trailer_line(line) {
                in_trailer = true;
            } else if !(in_trailer
                && (line.starts_with(' ') || line.starts_with('\t')))
            {
                in_trailer = false;
            }
            if in_trailer {
                trailer_like_count += 1;
            }
        }

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
        // spr 1.x wrote long section values, like a Pull Request URL,
        // on their own line below the label; without this check such a
        // line makes the section look like a trailer block, and the
        // section is not recognised (see `parse_legacy_message`).
        !Self::is_url_line(line)
            && lazy_regex::regex!(r"^[A-Za-z0-9][\w-]*\s*:\s*.+$")
                .is_match(line)
    }

    /// Whether a line is a bare URL, like "https://github.com/…" or
    /// "ssh://git@…": it matches `Key: value` with the URL's scheme as
    /// the key ("https") and "//…" as the value. Both trailer functions
    /// keep such a line out of their trailer matches: as body text it
    /// survives spr rewriting the message, as a trailer the URL would
    /// end up broken ("https: //…").
    fn is_url_line(line: &str) -> bool {
        lazy_regex::regex!(r"^[\w+.-]+://").is_match(line)
    }

    /// Parse trailer lines into key-value pairs.
    /// Returns (trailers, non_trailer_lines) where non_trailer_lines should be
    /// appended to the body.
    /// Supports multi-line trailers (RFC 822 style continuation lines).
    fn parse_trailers(lines: &[&str]) -> (Vec<(String, String)>, Vec<String>) {
        let mut trailers = Vec::new();
        let mut non_trailer_lines = Vec::new();
        let regex = lazy_regex::regex!(r"^([A-Za-z0-9][\w-]*)\s*:\s*(.*)$");

        for line in lines {
            if !Self::is_url_line(line)
                && let Some(caps) = regex.captures(line)
            {
                // New trailer line
                let key = caps.get(1).unwrap().as_str().to_string();
                let value = caps.get(2).unwrap().as_str().trim().to_string();
                trailers.push((key, value));
            } else if line.starts_with(' ') || line.starts_with('\t') {
                // Continuation line (folded line in RFC 822 style)
                if let Some(last_trailer) = trailers.last_mut() {
                    // Append to the previous trailer's value
                    last_trailer.1.push(' ');
                    last_trailer.1.push_str(line.trim());
                } else {
                    // Continuation line with no previous trailer - treat as non-trailer
                    non_trailer_lines.push(line.to_string());
                }
            } else if !line.trim().is_empty() {
                // Non-empty, non-trailer line - should be moved to body
                non_trailer_lines.push(line.to_string());
            }
        }

        (trailers, non_trailer_lines)
    }

    /// Parse a commit message written by spr 1.x, or return `None` if the
    /// message doesn't have the shape spr 1.x wrote.
    ///
    /// spr 1.x wrote its fields as labeled sections at the end of the
    /// message, in this order:
    ///
    /// ```text
    /// Test Plan: …
    ///
    /// Reviewers: …
    ///
    /// Reviewed By: …
    ///
    /// Pull Request: <url>
    /// ```
    ///
    /// Only that shape is recognised: the "Pull Request:" section must
    /// be there (every commit spr 1.x managed has one, from `spr diff`
    /// or `spr patch`), and it must be the last paragraph before the
    /// trailers, where spr 1.x always wrote it. A message with spr 2.0's
    /// trailers never looks like that, so it can't be mistaken for one
    /// of spr 1.x's.
    ///
    /// The sections spr still has become its trailers (including
    /// "Reviewed By:", which becomes `Reviewed-by`). Everything else -
    /// like the "Test Plan:" section, which spr 2.0 doesn't have - stays
    /// in the body, so nothing is lost when the message is converted.
    fn parse_legacy_message(title: &str, lines: &[&str]) -> Option<Self> {
        // The "Pull Request:" section: spr 1.x wrote it last.
        let (mut sections_end, section, pull_request) =
            Self::legacy_paragraph(lines, lines.len())?;
        if section != "Pull Request" {
            return None;
        }

        let mut reviewed_by = None;
        let mut reviewers = None;

        // Above it, spr 1.x wrote the "Reviewed By:" and "Reviewers:"
        // sections (at most one of each). The first paragraph that
        // isn't one of them - the "Test Plan:" section, the body - ends
        // the search; everything above `sections_end` is the body.
        while sections_end > 1 {
            let Some((end, section, value)) =
                Self::legacy_paragraph(lines, sections_end)
            else {
                break;
            };
            match section {
                "Reviewed By" if reviewed_by.is_none() => {
                    reviewed_by = Some(value)
                }
                "Reviewers" if reviewers.is_none() => reviewers = Some(value),
                _ => break,
            }
            sections_end = end;
        }

        let mut trailers = Vec::new();
        if let Some(reviewers) = reviewers {
            trailers.push(("Reviewers".to_string(), reviewers));
        }
        if let Some(reviewed_by) = reviewed_by {
            trailers.push(("Reviewed-by".to_string(), reviewed_by));
        }
        trailers.push(("Pull-request".to_string(), pull_request));

        Some(Self {
            title: title.to_string(),
            body: lines[1..sections_end].join("\n").trim().to_string(),
            trailers,
        })
    }

    /// The last paragraph of `lines[..end]`, if it starts with a legacy
    /// section: the index of its first line, the section's label, and
    /// its value. The value is the text after the label's colon,
    /// followed by the rest of the paragraph's lines - spr 1.x wrote
    /// long values, like a Pull Request URL, on the lines below the
    /// label. The paragraph never includes the title (line 0).
    fn legacy_paragraph(
        lines: &[&str],
        end: usize,
    ) -> Option<(usize, &'static str, String)> {
        // Skip blank lines at the end
        let mut end = end;
        while end > 1 && lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        if end <= 1 {
            return None;
        }

        // Find the paragraph's first line: the first non-blank line
        // after a blank one, or the title
        let mut start = end;
        while start > 1 && !lines[start - 1].trim().is_empty() {
            start -= 1;
        }

        let (section, value) = Self::legacy_section(lines[start])?;
        let mut value = value.to_string();
        for line in &lines[start + 1..end] {
            value.push('\n');
            value.push_str(line);
        }
        Some((start, section, value.trim().to_string()))
    }

    /// The legacy section a line starts, if any: the section's label
    /// ("Reviewers", "Reviewed By" or "Pull Request") and the text
    /// after the colon. spr 1.x recognised the labels in any case, and so
    /// does this.
    fn legacy_section(line: &str) -> Option<(&'static str, &str)> {
        let line = line.trim_start();
        let (label, value) = line.split_once(':')?;
        let section = match label.trim_end().to_ascii_lowercase().as_str() {
            "reviewers" => "Reviewers",
            "reviewed by" => "Reviewed By",
            "pull request" => "Pull Request",
            _ => return None,
        };
        Some((section, value.trim()))
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

    /// The ID spr gave the local commit (`Spr-Id` trailer), if any. A
    /// trailer that isn't a valid ID (see `is_valid_spr_id`) is ignored.
    pub fn spr_id(&self) -> Option<&str> {
        self.get_trailer(SPR_ID_TRAILER)
            .filter(|id| is_valid_spr_id(id))
    }

    /// Give the local commit a new ID (`Spr-Id` trailer), unless it has a
    /// valid one already. (An invalid one is replaced.) Returns whether it
    /// added one.
    pub fn ensure_spr_id(&mut self) -> bool {
        if self.spr_id().is_some() {
            return false;
        }
        self.set_trailer(SPR_ID_TRAILER.to_string(), new_spr_id());
        true
    }

    /// Update this (local) commit message from the message of its Pull
    /// Request on GitHub: take the title, the description and the trailers
    /// from there, i.e. both the user's trailers (from the description) and
    /// the ones spr maintains based on the Pull Request (`Pull-request`,
    /// `Reviewers`, `Reviewed-by`). Only the local-only trailers (`Spr-Id`,
    /// `Spr-Target`) are kept.
    ///
    /// If the trailers are the same apart from their order, their local
    /// order is kept, so that the commit isn't rewritten for no reason.
    pub fn update_from_pull_request(&mut self, pull_request: &CommitMessage) {
        self.title = pull_request.title.clone();
        self.body = pull_request.body.clone();

        let mut pull_request_trailers: Vec<(String, String)> = pull_request
            .trailers
            .iter()
            .filter(|(key, _)| !is_local_only_trailer(key))
            .cloned()
            .collect();
        let mut current_trailers: Vec<(String, String)> = self
            .trailers
            .iter()
            .filter(|(key, _)| !is_local_only_trailer(key))
            .cloned()
            .collect();
        pull_request_trailers.sort();
        current_trailers.sort();
        if pull_request_trailers == current_trailers {
            return;
        }

        let local_only_trailers = self
            .trailers
            .iter()
            .filter(|(key, _)| is_local_only_trailer(key))
            .cloned();
        self.trailers = pull_request
            .trailers
            .iter()
            .filter(|(key, _)| !is_local_only_trailer(key))
            .cloned()
            .chain(local_only_trailers)
            .collect();
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

    /// The description of the Pull Request: the body, followed by the
    /// user's trailers (those that spr doesn't manage, see `is_spr_trailer`).
    /// Hiding them would hide information from reviewers (e.g. who
    /// co-authored the change), and edits to them on GitHub can make their
    /// way back into the local commit (see `from_pull_request`).
    pub fn to_github_body(&self) -> String {
        self.body_with_trailers(|key| !is_spr_trailer(key))
    }

    /// The body of the commit that lands when the Pull Request is merged:
    /// the description, with the trailers of the Pull Request's message
    /// (built by `from_pull_request`, plus the ones that reflect the Pull
    /// Request) as one block after it, so Git recognises all of them as
    /// trailers. Local-only trailers are left out.
    pub fn to_github_body_for_merging(&self) -> String {
        self.body_with_trailers(|key| !is_local_only_trailer(key))
    }

    /// The body, followed by the trailers for which `include` returns true,
    /// as one block, unfolded
    fn body_with_trailers(&self, include: impl Fn(&str) -> bool) -> String {
        let trailers: Vec<String> = self
            .trailers
            .iter()
            .filter(|(key, _)| include(key))
            .map(|(key, value)| format!("{}: {}", key, unfold(value)))
            .collect();

        let mut result = self.body.clone();
        if !trailers.is_empty() {
            if !result.is_empty() {
                result.push_str("\n\n");
            }
            result.push_str(&trailers.join("\n"));
        }
        result
    }
}

/// Unfold a multi-line trailer value: split by newlines, trim each line,
/// filter empty lines, and join with single spaces
fn unfold(value: &str) -> String {
    value
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
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
                writeln!(f, "{}: {}", key, unfold(value))?;
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
    fn test_parse_legacy_message_with_all_sections() {
        let msg = CommitMessage::parse(
            "Title\n\
             \n\
             The summary, as spr 1.x wrote it.\n\
             \n\
             Test Plan: pressed all the buttons\n\
             \n\
             Reviewers: alice, bob\n\
             \n\
             Reviewed By: alice\n\
             \n\
             Pull Request: https://github.com/owner/repo/pull/7",
        );
        assert_eq!(msg.title(), "Title");
        // spr 2.0 has no "Test Plan" section: its text stays in the body
        assert_eq!(
            msg.body(),
            "The summary, as spr 1.x wrote it.\n\n\
             Test Plan: pressed all the buttons"
        );
        assert_eq!(msg.get_trailer("Reviewers"), Some("alice, bob"));
        assert_eq!(msg.get_trailer("Reviewed-by"), Some("alice"));
        assert_eq!(
            msg.get_trailer("Pull-request"),
            Some("https://github.com/owner/repo/pull/7")
        );

        // When spr rewrites the message, the sections become trailers
        assert_eq!(
            msg.to_string(),
            "Title\n\n\
             The summary, as spr 1.x wrote it.\n\n\
             Test Plan: pressed all the buttons\n\n\
             Reviewers: alice, bob\n\
             Reviewed-by: alice\n\
             Pull-request: https://github.com/owner/repo/pull/7\n"
        );
        // ... and the rewritten message parses to the same thing
        assert_eq!(CommitMessage::parse(&msg.to_string()), msg);
    }

    /// A Pull Request URL so long that spr 1.x wrote it below the label
    #[test]
    fn test_parse_legacy_pull_request_url_below_the_label() {
        let msg = CommitMessage::parse(
            "Title\n\nBody\n\nPull Request:\n\
             https://github.com/very-long-organisation/\
             their-repository/pull/123456",
        );
        assert_eq!(msg.body(), "Body");
        assert_eq!(
            msg.get_trailer("Pull-request"),
            Some(
                "https://github.com/very-long-organisation/\
                 their-repository/pull/123456"
            )
        );
    }

    /// spr 1.x recognised its section labels in any case
    #[test]
    fn test_parse_legacy_sections_case_insensitive() {
        let msg = CommitMessage::parse(
            "Title\n\nBody\n\nREVIEWERS: alice\n\n\
             pull request: https://github.com/owner/repo/pull/2",
        );
        assert_eq!(msg.body(), "Body");
        assert_eq!(msg.get_trailer("Reviewers"), Some("alice"));
        assert_eq!(
            msg.get_trailer("Pull-request"),
            Some("https://github.com/owner/repo/pull/2")
        );
    }

    /// Lines like spr 1.x's sections, but in the middle of the body, are
    /// body text: spr 1.x only ever wrote them at the end of the message
    #[test]
    fn test_labels_in_body_text_stay_in_the_body() {
        let msg = CommitMessage::parse(
            "Title\n\n\
             Pull Request: requests are handled by triage now.\n\
             Reviewed By: whoever gets to it.\n\
             Reviewers: see the wiki.\n\
             \n\
             The rest of the description.\n\
             \n\
             Signed-off-by: Alice <alice@example.com>",
        );
        assert_eq!(msg.get_trailer("Pull-request"), None);
        assert_eq!(msg.get_trailer("Reviewed-by"), None);
        assert_eq!(msg.get_trailer("Reviewers"), None);
        assert_eq!(
            msg.body(),
            "Pull Request: requests are handled by triage now.\n\
             Reviewed By: whoever gets to it.\n\
             Reviewers: see the wiki.\n\
             \n\
             The rest of the description."
        );
        assert_eq!(
            msg.get_trailer("Signed-off-by"),
            Some("Alice <alice@example.com>")
        );
    }

    /// Labels directly above the trailer block are in the position spr
    /// 1.x wrote its sections, so they are read as spr 1.x's sections -
    /// and spr 1.x would have read them the same way
    #[test]
    fn test_labels_above_trailers_are_legacy_sections() {
        let msg = CommitMessage::parse(
            "Title\n\n\
             Pull Request: requests are handled by triage now.\n\
             \n\
             Signed-off-by: Alice <alice@example.com>",
        );
        assert_eq!(msg.body(), "");
        assert_eq!(
            msg.get_trailer("Pull-request"),
            Some("requests are handled by triage now.")
        );
        assert_eq!(
            msg.get_trailer("Signed-off-by"),
            Some("Alice <alice@example.com>")
        );
    }

    /// The trailing "Pull Request:" line is what makes the message one
    /// spr 1.x wrote: whatever the value is, it is read with spr 1.x's
    /// rules, like spr 1.x itself would have read it
    #[test]
    fn test_trailing_pull_request_line_makes_a_legacy_message() {
        let msg = CommitMessage::parse(
            "Title\n\nThe Pull Request: requests are handled by triage now.\n\n\
             Reviewed By: whoever\n\n\
             Pull Request: not a URL",
        );
        assert_eq!(
            msg.body(),
            "The Pull Request: requests are handled by triage now."
        );
        assert_eq!(msg.get_trailer("Reviewed-by"), Some("whoever"));
        assert_eq!(msg.get_trailer("Pull-request"), Some("not a URL"));
    }

    /// A "Reviewers" paragraph at the end, without a "Pull Request:"
    /// section, is not a message spr 1.x wrote: it is a trailer block
    #[test]
    fn test_reviewers_without_pull_request_section_is_a_trailer() {
        let msg = CommitMessage::parse("Title\n\nBody\n\nReviewers: alice");
        assert_eq!(msg.body(), "Body");
        assert_eq!(msg.get_trailer("Reviewers"), Some("alice"));
    }

    /// "Reviewed By:" without a "Pull Request:" section is not a message
    /// spr 1.x wrote, and it can't be a trailer either (the key has a
    /// space): it is body text
    #[test]
    fn test_reviewed_by_without_pull_request_section_stays_in_body() {
        let msg = CommitMessage::parse("Title\n\nBody\n\nReviewed By: alice");
        assert_eq!(msg.body(), "Body\n\nReviewed By: alice");
        assert_eq!(msg.get_trailer("Reviewed-by"), None);
    }

    /// Trailers below the old sections, e.g. a Signed-off-by a user added
    /// to a message spr 1.x wrote (`git commit --amend -s` appends them
    /// at the end): the sections are still recognised
    #[test]
    fn test_legacy_message_with_trailers_below_the_sections() {
        let msg = CommitMessage::parse(
            "Title\n\nSummary.\n\n\
             Pull Request: https://github.com/owner/repo/pull/4\n\n\
             Signed-off-by: Alice <alice@example.com>",
        );
        assert_eq!(msg.body(), "Summary.");
        assert_eq!(
            msg.get_trailer("Pull-request"),
            Some("https://github.com/owner/repo/pull/4")
        );
        assert_eq!(
            msg.get_trailer("Signed-off-by"),
            Some("Alice <alice@example.com>")
        );
    }

    /// A message with spr 2.0's Pull-request trailer is not parsed as a
    /// legacy one, even if it also has old-style lines, which then stay
    /// in the body
    #[test]
    fn test_pull_request_trailer_disables_legacy_parsing() {
        let msg = CommitMessage::parse(
            "Title\n\nBody\n\nPull Request: not read\n\n\
             Pull-request: https://github.com/owner/repo/pull/4",
        );
        assert_eq!(msg.body(), "Body\n\nPull Request: not read");
        assert_eq!(
            msg.get_trailer("Pull-request"),
            Some("https://github.com/owner/repo/pull/4")
        );
    }

    #[test]
    fn test_parse_legacy_sections_with_crlf() {
        let msg = CommitMessage::parse(
            "Title\r\n\r\nBody\r\n\r\nReviewers: alice\r\n\r\n\
             Pull Request: https://github.com/owner/repo/pull/3\r\n",
        );
        assert_eq!(msg.body(), "Body");
        assert_eq!(msg.get_trailer("Reviewers"), Some("alice"));
        assert_eq!(
            msg.get_trailer("Pull-request"),
            Some("https://github.com/owner/repo/pull/3")
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

    #[test]
    fn test_non_trailer_lines_moved_to_body() {
        let msg = CommitMessage::parse(
            "Title\n\nBody text\n\nSigned-off-by: Alice\nSome random note\nReviewed-by: Bob",
        );
        assert_eq!(msg.title(), "Title");
        assert_eq!(msg.body(), "Body text\n\nSome random note");
        assert_eq!(msg.trailers().len(), 2);
        assert_eq!(msg.get_trailer("Signed-off-by"), Some("Alice"));
        assert_eq!(msg.get_trailer("Reviewed-by"), Some("Bob"));

        // When serialized, it should produce clean format
        let serialized = msg.to_string();
        assert_eq!(
            serialized,
            "Title\n\nBody text\n\nSome random note\n\nSigned-off-by: Alice\nReviewed-by: Bob\n"
        );
    }

    #[test]
    fn test_multiline_trailers() {
        // Multi-line trailer using RFC 822 style continuation (leading whitespace)
        let msg = CommitMessage::parse(
            "Title\n\nBody\n\nSigned-off-by: Some Person\n <with-a-long-email@example.com>\nReviewed-by: Bob",
        );
        assert_eq!(msg.title(), "Title");
        assert_eq!(msg.body(), "Body");
        assert_eq!(msg.trailers().len(), 2);
        assert_eq!(
            msg.get_trailer("Signed-off-by"),
            Some("Some Person <with-a-long-email@example.com>")
        );
        assert_eq!(msg.get_trailer("Reviewed-by"), Some("Bob"));

        // When serialized, it becomes single-line (we don't preserve multi-line format)
        let serialized = msg.to_string();
        assert!(serialized.contains(
            "Signed-off-by: Some Person <with-a-long-email@example.com>\n"
        ));
    }

    #[test]
    fn test_trailer_with_multiple_continuation_lines() {
        let msg = CommitMessage::parse(
            "Title\n\nBody\n\nSigned-off-by: Some Person\n <a@example.com>\n (on behalf of Org)",
        );
        assert_eq!(msg.body(), "Body");
        assert_eq!(
            msg.get_trailer("Signed-off-by"),
            Some("Some Person <a@example.com> (on behalf of Org)")
        );
    }

    #[test]
    fn test_indented_lines_without_trailer_are_body() {
        let msg = CommitMessage::parse(
            "Title\n\nBody\n\nSome-key: value\nplain text\n indented\n indented",
        );
        assert_eq!(
            msg.body(),
            "Body\n\nSome-key: value\nplain text\n indented\n indented"
        );
        assert!(msg.trailers().is_empty());
    }

    #[test]
    fn test_trailer_value_with_newlines_gets_unfolded() {
        let mut msg =
            CommitMessage::new("Title".to_string(), "Body".to_string());
        msg.set_trailer("Foo".to_string(), "line1\nline2\nline3".to_string());

        // The value should be stored as-is
        assert_eq!(msg.get_trailer("Foo"), Some("line1\nline2\nline3"));

        // But when serialized, newlines should be replaced with spaces
        let serialized = msg.to_string();
        assert!(serialized.contains("Foo: line1 line2 line3\n"));
        assert!(!serialized.contains("Foo: line1\nline2"));

        // When re-parsed, the unfolded value should be preserved
        let reparsed = CommitMessage::parse(&serialized);
        assert_eq!(reparsed.get_trailer("Foo"), Some("line1 line2 line3"));
    }

    #[test]
    fn test_trailer_value_whitespace_normalization() {
        let mut msg =
            CommitMessage::new("Title".to_string(), "Body".to_string());

        // Various whitespace scenarios
        msg.set_trailer(
            "A".to_string(),
            "  leading and trailing  ".to_string(),
        );
        msg.set_trailer("B".to_string(), "line1  \n  line2".to_string());
        msg.set_trailer("C".to_string(), "line1\n\n\nline2".to_string()); // multiple blank lines
        msg.set_trailer(
            "D".to_string(),
            "  word1   word2  \n  word3  ".to_string(),
        );

        let serialized = msg.to_string();

        // Leading/trailing whitespace trimmed
        assert!(serialized.contains("A: leading and trailing\n"));

        // Lines trimmed and joined with single space
        assert!(serialized.contains("B: line1 line2\n"));

        // Empty lines filtered out
        assert!(serialized.contains("C: line1 line2\n"));

        // Multiple spaces within a line preserved, but line boundaries normalized
        assert!(serialized.contains("D: word1   word2 word3\n"));
    }

    /// The Pull Request's message as `GitHub::get_pull_request` builds it
    fn pull_request_message(
        title: &str,
        description: &str,
        pull_request_trailers: &[(&str, &str)],
    ) -> CommitMessage {
        let mut message =
            CommitMessage::from_pull_request(title.into(), description);
        for (key, value) in pull_request_trailers {
            message.set_trailer(key.to_string(), value.to_string());
        }
        message
    }

    #[test]
    fn test_update_from_pull_request() {
        let mut local = CommitMessage::parse(
            "Old title\n\nOld body\n\n\
             Signed-off-by: Alice <alice@example.com>\n\
             Reviewers: bob\n\
             Pull-request: https://github.com/o/r/pull/1\n\
             Co-Authored-By: Carol <carol@example.com>",
        );
        let github = pull_request_message(
            "New title",
            "New body\n\n\
             Signed-off-by: Alice <alice@example.com>\n\
             Co-Authored-By: Carol <carol@example.com>\n\
             Co-Authored-By: Dave <dave@example.com>",
            &[
                ("Pull-request", "https://github.com/o/r/pull/1"),
                ("Reviewers", "bob, dave"),
                ("Reviewed-by", "dave"),
            ],
        );

        local.update_from_pull_request(&github);

        assert_eq!(local.title(), "New title");
        assert_eq!(local.body(), "New body");
        // The user's trailers come from the description, spr's from GitHub
        assert_eq!(
            local.to_string(),
            "New title\n\nNew body\n\n\
             Signed-off-by: Alice <alice@example.com>\n\
             Co-Authored-By: Carol <carol@example.com>\n\
             Co-Authored-By: Dave <dave@example.com>\n\
             Pull-request: https://github.com/o/r/pull/1\n\
             Reviewers: bob, dave\n\
             Reviewed-by: dave\n"
        );
    }

    #[test]
    fn test_update_from_pull_request_removes_trailer_removed_on_github() {
        let mut local = CommitMessage::parse(
            "Title\n\nBody\n\n\
             Signed-off-by: Alice <alice@example.com>\n\
             Pull-request: https://github.com/o/r/pull/1",
        );
        let github = pull_request_message(
            "Title",
            "Body",
            &[("Pull-request", "https://github.com/o/r/pull/1")],
        );

        local.update_from_pull_request(&github);

        assert_eq!(local.get_trailer("Signed-off-by"), None);
        assert!(local.get_trailer("Pull-request").is_some());
    }

    #[test]
    fn test_update_from_pull_request_keeps_order_of_same_trailers() {
        let text = "Title\n\nBody\n\n\
                    Pull-request: https://github.com/o/r/pull/1\n\
                    Signed-off-by: Alice <alice@example.com>\n\
                    Reviewers: bob";
        let mut local = CommitMessage::parse(text);
        let github = pull_request_message(
            "Title",
            &local.to_github_body(),
            &[
                ("Pull-request", "https://github.com/o/r/pull/1"),
                ("Reviewers", "bob"),
            ],
        );

        local.update_from_pull_request(&github);

        assert_eq!(local, CommitMessage::parse(text));
    }

    #[test]
    fn test_pull_request_round_trip() {
        // A local commit, its Pull Request's description, and the message
        // that spr builds from the Pull Request: updating the local commit
        // from it changes nothing
        let mut local = CommitMessage::parse(
            "Title\n\nBody\n\n\
             Co-authored-by: Carol <carol@example.com>\n\
             Signed-off-by: Alice <alice@example.com>\n\
             Reviewers: bob\n\
             Spr-Target: github:o/r:release\n\
             Pull-request: https://github.com/o/r/pull/1",
        );
        local.ensure_spr_id();
        let description = local.to_github_body();
        assert_eq!(
            description,
            "Body\n\n\
             Co-authored-by: Carol <carol@example.com>\n\
             Signed-off-by: Alice <alice@example.com>"
        );

        let github = pull_request_message(
            "Title",
            &description,
            &[
                ("Pull-request", "https://github.com/o/r/pull/1"),
                ("Reviewers", "bob"),
            ],
        );
        let before = local.clone();
        local.update_from_pull_request(&github);
        assert_eq!(local, before);
    }

    #[test]
    fn test_from_pull_request() {
        // The trailers at the end of the description are the user's
        // trailers; ones that spr manages are ignored
        let message = CommitMessage::from_pull_request(
            "Title".into(),
            "Body\n\nMore body\n\n\
             Co-authored-by: Carol <carol@example.com>\n\
             Reviewed-by: mallory\n\
             Spr-Id: 7f3a91c0d2e84b5f9a6c1e0b3d7f2a48",
        );
        assert_eq!(message.title(), "Title");
        assert_eq!(message.body(), "Body\n\nMore body");
        assert_eq!(
            message.trailers(),
            [(
                "Co-authored-by".to_string(),
                "Carol <carol@example.com>".to_string()
            )]
        );

        // Without trailers, the description is the body
        let message = CommitMessage::from_pull_request(
            "Title".into(),
            "Body\n\nNot: a trailer block, because\nmost of its lines\naren't trailers",
        );
        assert!(message.trailers().is_empty());
    }

    /// spr 1.x never wrote its sections into a Pull Request's description,
    /// so a description is never parsed as one of spr 1.x's messages: a
    /// line like this is description text, and `spr amend` and `spr land`
    /// keep it in the commit message
    #[test]
    fn test_from_pull_request_keeps_legacy_labels_in_the_body() {
        let message = CommitMessage::from_pull_request(
            "Title".to_string(),
            "Body\n\nPull Request: see #12 for context.",
        );
        assert_eq!(
            message.body(),
            "Body\n\nPull Request: see #12 for context."
        );
        assert!(message.trailers().is_empty());
    }

    /// A URL on a line of its own in a trailer block is not a trailer:
    /// it would be parsed with the key "https", and the rewritten
    /// message would break the URL
    #[test]
    fn test_url_in_trailer_block_stays_in_the_body() {
        let msg = CommitMessage::parse(
            "Title\n\nBody\n\nFixes: foo\nhttps://example.com/x",
        );
        assert_eq!(msg.get_trailer("Fixes"), Some("foo"));
        assert_eq!(msg.body(), "Body\n\nhttps://example.com/x");
    }

    #[test]
    fn test_update_from_pull_request_removes_stale_trailers() {
        let mut local = CommitMessage::parse(
            "Title\n\nBody\n\n\
             Reviewers: bob\n\
             Reviewed-by: bob\n\
             Pull-request: https://github.com/o/r/pull/1",
        );
        // The Pull Request has no reviewers anymore, and isn't approved
        let github = pull_request_message(
            "Title",
            "Body",
            &[("Pull-request", "https://github.com/o/r/pull/1")],
        );

        local.update_from_pull_request(&github);

        assert_eq!(local.get_trailer("Reviewers"), None);
        assert_eq!(local.get_trailer("Reviewed-by"), None);
        assert!(local.get_trailer("Pull-request").is_some());
    }

    #[test]
    fn test_new_spr_id() {
        let ids: std::collections::HashSet<String> =
            (0..1000).map(|_| new_spr_id()).collect();

        assert_eq!(ids.len(), 1000);
        assert!(ids.iter().all(|id| is_valid_spr_id(id)));
    }

    #[test]
    fn test_invalid_spr_id_is_ignored_and_replaced() {
        for invalid in [
            "../../heads/master",
            "7F3A91C0D2E84B5F9A6C1E0B3D7F2A48",
            "7f3a91c0d2e84b5f9a6c1e0b3d7f2a4",
            "7f3a91c0d2e84b5f9a6c1e0b3d7f2a48/x",
            "7f3a91c0d2e84b5f9a6c1e0b3d7f2a4g",
        ] {
            let mut message =
                CommitMessage::parse(&format!("Title\n\nSpr-Id: {invalid}"));
            assert_eq!(message.spr_id(), None, "{invalid}");

            assert!(message.ensure_spr_id());
            assert!(message.spr_id().is_some());
            assert_eq!(message.trailers().len(), 1);
        }
    }

    #[test]
    fn test_ensure_spr_id() {
        let mut message = CommitMessage::parse(
            "Title\n\nBody\n\nSigned-off-by: Alice <alice@example.com>",
        );
        assert_eq!(message.spr_id(), None);

        assert!(message.ensure_spr_id());
        let id = message.spr_id().unwrap().to_string();
        assert_eq!(id.len(), 32);

        // An existing ID is kept, and survives serialising and parsing
        assert!(!message.ensure_spr_id());
        let message = CommitMessage::parse(&message.to_string());
        assert_eq!(message.spr_id(), Some(id.as_str()));
        assert_eq!(
            message.get_trailer("Signed-off-by"),
            Some("Alice <alice@example.com>")
        );
    }

    #[test]
    fn test_spr_id_not_in_pull_request() {
        let mut message = CommitMessage::parse(
            "Title\n\nBody\n\n\
             Pull-request: https://github.com/o/r/pull/1",
        );
        message.ensure_spr_id();

        assert!(!message.to_github_body().contains("Spr-Id"));
        assert!(!message.to_github_body_for_merging().contains("Spr-Id"));
    }

    #[test]
    fn test_github_body_for_merging() {
        let mut message = CommitMessage::parse(
            "Title\n\nBody\n\n\
             Reviewers: alice,\n  bob\n\
             Signed-off-by: Carol <carol@example.com>",
        );
        message.ensure_spr_id();
        message.set_trailer(
            "Pull-request".into(),
            "https://github.com/o/r/pull/1".into(),
        );
        message.set_trailer("Reviewed-by".into(), "alice".into());

        // All trailers but the local-only ones, as one block (so Git sees
        // all of them as trailers), unfolded
        assert_eq!(
            message.to_github_body_for_merging(),
            "Body\n\n\
             Reviewers: alice, bob\n\
             Signed-off-by: Carol <carol@example.com>\n\
             Pull-request: https://github.com/o/r/pull/1\n\
             Reviewed-by: alice"
        );

        // Without a body, just the trailers
        let mut message = CommitMessage::new("Title".into(), String::new());
        message.set_trailer("Reviewed-by".into(), "alice".into());
        assert_eq!(message.to_github_body_for_merging(), "Reviewed-by: alice");
    }

    #[test]
    fn test_spr_target_stays_local() {
        let mut local = CommitMessage::parse(
            "Title\n\nBody\n\n\
             Spr-Target: github:o/r:release\n\
             Pull-request: https://github.com/o/r/pull/1",
        );
        assert!(!local.to_github_body().contains("Spr-Target"));
        assert!(!local.to_github_body_for_merging().contains("Spr-Target"));

        let github = CommitMessage::new("New title".into(), "Body".into());
        local.update_from_pull_request(&github);
        assert_eq!(
            local.get_trailer(SPR_TARGET_TRAILER),
            Some("github:o/r:release")
        );
    }

    #[test]
    fn test_spr_id_kept_when_updating_from_pull_request() {
        let mut local = CommitMessage::parse("Title\n\nBody");
        local.ensure_spr_id();
        let id = local.spr_id().unwrap().to_string();

        let github = CommitMessage::new("New title".into(), "Body".into());
        local.update_from_pull_request(&github);

        assert_eq!(local.spr_id(), Some(id.as_str()));
    }
}
