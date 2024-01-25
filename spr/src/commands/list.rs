/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::error::Error;
use crate::error::Result;
use graphql_client::{GraphQLQuery, Response};

#[allow(clippy::upper_case_acronyms)]
type URI = String;
#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.docs.graphql",
    query_path = "src/gql/open_reviews.graphql",
    response_derives = "Debug"
)]
pub struct SearchQuery;

pub async fn list(
    config: &crate::config::Config,
) -> Result<()> {
    let variables = search_query::Variables {
        query: format!(
            "repo:{}/{} is:open is:pr author:@me archived:false",
            config.owner, config.repo
        ),
    };
    let request_body = SearchQuery::build_query(variables);
    let response_body: Response<search_query::ResponseData> = octocrab::instance()
        .post("graphql",Some(&request_body))
        .await?;

    print_pr_info(response_body).ok_or_else(|| Error::new("unexpected error"))
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
        let dummy: String;
        let decision = match pr.review_decision {
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
