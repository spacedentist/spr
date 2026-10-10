use color_eyre::eyre::{Result, WrapErr as _, eyre};
use graphql_client::{GraphQLQuery, Response};

use crate::github::{ReviewStatus, review_status_from_reviews};

#[allow(clippy::upper_case_acronyms)]
type URI = String;
#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.docs.graphql",
    query_path = "src/gql/open_reviews.graphql",
    response_derives = "Debug"
)]
pub struct SearchQuery;

pub async fn list(config: &crate::config::Config) -> Result<()> {
    let variables = search_query::Variables {
        query: format!(
            "repo:{}/{} is:open is:pr author:@me archived:false",
            config.owner, config.repo
        ),
    };
    let request_body = SearchQuery::build_query(variables);
    let response_body: Response<search_query::ResponseData> =
        octocrab::instance()
            .post("/graphql", Some(&request_body))
            .await
            .wrap_err("Searching for open PRs".to_string())?;

    print_pr_info(response_body).ok_or_else(|| eyre!("unexpected error"))
}

fn print_pr_info(
    response_body: Response<search_query::ResponseData>,
) -> Option<()> {
    let term = console::Term::stdout();
    for pr in response_body.data?.search.nodes? {
        let pr = match pr {
            Some(crate::commands::list::search_query::SearchQuerySearchNodes::PullRequest(pr)) => pr,
            _ => continue,
        };
        // Without required reviews, GitHub has no review decision: go by
        // the reviews instead
        let review_decision = pr.review_decision.or_else(|| {
            use search_query::{
                PullRequestReviewDecision as Decision,
                PullRequestReviewState as State,
            };
            let reviews: Vec<ReviewStatus> = pr
                .latest_opinionated_reviews
                .iter()
                .flat_map(|reviews| &reviews.nodes)
                .flatten()
                .flatten()
                .map(|review| match review.state {
                    State::APPROVED => ReviewStatus::Approved,
                    State::CHANGES_REQUESTED => ReviewStatus::Rejected,
                    _ => ReviewStatus::Requested,
                })
                .collect();
            match review_status_from_reviews(&reviews)? {
                ReviewStatus::Approved => Some(Decision::APPROVED),
                ReviewStatus::Rejected => Some(Decision::CHANGES_REQUESTED),
                ReviewStatus::Requested => None,
            }
        });
        let dummy: String;
        let decision = match review_decision {
            Some(search_query::PullRequestReviewDecision::APPROVED) => {
                console::style("Accepted").green()
            }
            Some(
                search_query::PullRequestReviewDecision::CHANGES_REQUESTED,
            ) => console::style("Changes Requested").red(),
            None
            | Some(search_query::PullRequestReviewDecision::REVIEW_REQUIRED) => {
                console::style("Pending")
            }
            Some(search_query::PullRequestReviewDecision::Other(d)) => {
                dummy = d;
                console::style(dummy.as_str())
            }
        };
        term.write_line(&format!(
            "{} {} {}",
            decision,
            console::style(&pr.title).bold(),
            console::style(&pr.url).dim(),
        ))
        .ok()?;
    }
    Some(())
}
