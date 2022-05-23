/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::{error::Result, git::PreparedCommit, message::MessageSection};

pub fn output(icon: &str, text: &str) -> Result<()> {
    let term = console::Term::stdout();

    let bullet = format!("  {}  ", icon);
    let indent = console::measure_text_width(&bullet);
    let indent_string = " ".repeat(indent);
    let options = textwrap::Options::new((term.size().1 as usize) - indent * 2)
        .initial_indent(&bullet)
        .subsequent_indent(&indent_string);

    term.write_line(&textwrap::wrap(text.trim(), &options).join("\n"))?;
    Ok(())
}

pub fn write_commit_title(prepared_commit: &PreparedCommit) -> Result<()> {
    let term = console::Term::stdout();
    term.write_line(&format!(
        "{} {}",
        console::style(&prepared_commit.short_id).italic(),
        console::style(
            prepared_commit
                .message
                .get(&MessageSection::Title)
                .map(|s| &s[..])
                .unwrap_or("(untitled)"),
        )
        .yellow()
    ))?;
    Ok(())
}
