use color_eyre::eyre::Result;

use crate::git::PreparedCommit;

pub fn output(icon: &str, text: &str) -> Result<()> {
    let term = console::Term::stdout();

    let bullet = format!("  {}  ", icon);
    let indent = console::measure_text_width(&bullet);
    let indent_string = " ".repeat(indent);
    let options = textwrap::Options::new((term.size().1 as usize) - indent * 2)
        .initial_indent(&bullet)
        .subsequent_indent(&indent_string)
        .break_words(false)
        .word_separator(textwrap::WordSeparator::AsciiSpace)
        .word_splitter(textwrap::WordSplitter::NoHyphenation);

    term.write_line(&textwrap::wrap(text.trim(), &options).join("\n"))?;
    Ok(())
}

pub fn write_commit_title(prepared_commit: &PreparedCommit) -> Result<()> {
    let term = console::Term::stdout();
    let title = prepared_commit.message.title();
    let title_display = if title.is_empty() {
        "(untitled)"
    } else {
        title
    };
    term.write_line(&format!(
        "{} {}",
        console::style(&prepared_commit.short_id).italic(),
        console::style(title_display).yellow()
    ))?;
    Ok(())
}
