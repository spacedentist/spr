use color_eyre::eyre::{Result, WrapErr as _};

use crate::forge::{Forge, ReviewStatus};

pub async fn list(gh: &impl Forge) -> Result<()> {
    let pull_requests = gh
        .list_open_pull_requests()
        .await
        .wrap_err("Searching for open PRs".to_string())?;

    let term = console::Term::stdout();
    for pr in pull_requests {
        let decision = match pr.review_status {
            Some(ReviewStatus::Approved) => console::style("Accepted").green(),
            Some(ReviewStatus::Rejected) => {
                console::style("Changes Requested").red()
            }
            None | Some(ReviewStatus::Requested) => console::style("Pending"),
        };
        term.write_line(&format!(
            "{} {} {}",
            decision,
            console::style(&pr.title).bold(),
            console::style(&pr.url).dim(),
        ))?;
    }
    Ok(())
}
