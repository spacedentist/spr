//! Plumbing commands: low-level operations on Git objects, for use in
//! scripts. They don't move refs, push, fetch or talk to GitHub, and don't
//! need spr to be configured. (Experimental: the interface may still change.)

use std::io::Write;

use color_eyre::eyre::{Report, Result, WrapErr as _, eyre};
use git2::Oid;
use serde_json::json;

use crate::{
    git::Git,
    land_check::{self, LandCheck},
    message::CommitMessage,
    pr_commits::{
        self, BaseOutdated, CommitInfo, Conflict, Parent, PullRequestCommits,
    },
};

#[derive(Debug, clap::Parser)]
pub struct PlumbingOptions {
    #[clap(subcommand)]
    command: PlumbingCommand,
}

#[derive(Debug, clap::Subcommand)]
enum PlumbingCommand {
    /// Create the commits that make a Pull Request reflect a local commit.
    /// Prints the new head and base commit of the Pull Request.
    CommitPr(CommitPrOptions),

    /// List the commits between the target and a commit (default: HEAD),
    /// from bottom to top: commit, parent, and Pull-request trailer (or -)
    Stack(StackOptions),

    /// Check that merging a Pull Request into the target gives the same
    /// tree as applying the local commits to it. Prints that tree.
    LandCheck(LandCheckOptions),
}

impl PlumbingCommand {
    fn json(&self) -> bool {
        match self {
            PlumbingCommand::CommitPr(opts) => opts.json,
            PlumbingCommand::Stack(opts) => opts.json,
            PlumbingCommand::LandCheck(opts) => opts.json,
        }
    }
}

#[derive(Debug, clap::Parser)]
pub struct LandCheckOptions {
    /// The commit on the target branch the Pull Request would be merged into
    #[clap(long)]
    target: String,

    /// The head of the Pull Request
    #[clap(long)]
    pr_head: String,

    /// The (top) local commit
    #[clap(long)]
    local: String,

    /// The commit the local commits are based on: the changes of
    /// <since>..<local> are applied to the target [default: parent of
    /// --local]
    #[clap(long)]
    since: Option<String>,

    /// Output JSON
    #[clap(long)]
    json: bool,
}

#[derive(Debug, clap::Parser)]
pub struct StackOptions {
    /// The commit on the target branch the stack is based on
    #[clap(long)]
    target: String,

    /// The top commit of the stack
    #[clap(default_value = "HEAD")]
    commit: String,

    /// Output JSON, including the title and all trailers of each commit
    #[clap(long)]
    json: bool,
}

#[derive(Debug, clap::Parser)]
pub struct CommitPrOptions {
    /// The commit the Pull Request is currently based on
    #[clap(long)]
    base: String,

    /// The commit on the target branch the change is based on
    #[clap(long)]
    target: String,

    /// The current head of the Pull Request [default: --base, i.e. a new
    /// Pull Request]
    #[clap(long)]
    head: Option<String>,

    /// The local commit: shorthand for --tree <local>: --base-tree <local>~:
    #[clap(long, required_unless_present = "tree", conflicts_with = "tree")]
    local: Option<String>,

    /// The tree the head of the Pull Request should have
    #[clap(long, requires = "base_tree")]
    tree: Option<String>,

    /// The tree the base of the Pull Request should have
    #[clap(long, requires = "tree")]
    base_tree: Option<String>,

    /// Apply the change (from the base tree to the tree) onto --target
    #[clap(long)]
    cherry_pick: bool,

    /// Don't add commits to the base; fail if it doesn't have the base tree
    /// or doesn't contain --target
    #[clap(long)]
    fixed_base: bool,

    /// Message of the new head commit (required if one gets created)
    #[clap(long, short = 'm')]
    message: Option<String>,

    /// Message of the new base commit
    #[clap(long)]
    base_message: Option<String>,

    /// Commit whose author the new commits credit [default: --local (and its
    /// parent for the base commit), otherwise the current user]
    #[clap(long)]
    author_from: Option<String>,

    /// Don't create any commits, only print which ones would be created
    #[clap(long)]
    plan: bool,

    /// Output JSON
    #[clap(long)]
    json: bool,
}

/// An error of a plumbing command with a machine-readable kind and exit code
#[derive(Debug)]
struct PlumbingError {
    kind: &'static str,
    exit_code: i32,
    message: String,
}

impl std::fmt::Display for PlumbingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PlumbingError {}

/// The kind and exit code of an error
fn classify(error: &Report) -> (&'static str, i32) {
    if let Some(error) = error.downcast_ref::<PlumbingError>() {
        (error.kind, error.exit_code)
    } else if error.downcast_ref::<BaseOutdated>().is_some() {
        ("base-outdated", 2)
    } else if error.downcast_ref::<Conflict>().is_some() {
        ("conflict", 4)
    } else {
        ("error", 1)
    }
}

/// The JSON object reporting an error
fn error_json(error: &Report) -> serde_json::Value {
    let (kind, _) = classify(error);
    json!({ "error": { "kind": kind, "message": format!("{error:#}") } })
}

pub fn plumbing(opts: PlumbingOptions, git: &Git) -> Result<()> {
    let json = opts.command.json();
    let mut stdout = std::io::stdout().lock();

    match run(opts.command, git, &mut stdout) {
        Ok(()) => Ok(()),
        Err(error) => {
            // Errors go to stderr, and with --json, also as a JSON object to
            // stdout, so scripts parsing stdout always get JSON.
            if json {
                writeln!(stdout, "{}", error_json(&error))?;
            }
            eprintln!("error: {error:#}");
            std::process::exit(classify(&error).1);
        }
    }
}

fn run(command: PlumbingCommand, git: &Git, out: &mut dyn Write) -> Result<()> {
    match command {
        PlumbingCommand::CommitPr(opts) => commit_pr(opts, git, out),
        PlumbingCommand::Stack(opts) => stack(opts, git, out),
        PlumbingCommand::LandCheck(opts) => land_check(opts, git, out),
    }
}

fn find_commit(git: &Git, spec: &str) -> Result<Oid> {
    Ok(git
        .repo()
        .revparse_single(spec)
        .and_then(|object| object.peel_to_commit())
        .wrap_err_with(|| format!("'{spec}' is not a commit"))?
        .id())
}

fn find_tree(git: &Git, spec: &str) -> Result<Oid> {
    Ok(git
        .repo()
        .revparse_single(spec)
        .and_then(|object| object.peel_to_tree())
        .wrap_err_with(|| format!("'{spec}' is not a tree"))?
        .id())
}

fn commit_pr(
    opts: CommitPrOptions,
    git: &Git,
    out: &mut dyn Write,
) -> Result<()> {
    let base = find_commit(git, &opts.base)?;
    let target = find_commit(git, &opts.target)?;
    let head = opts
        .head
        .as_deref()
        .map(|spec| find_commit(git, spec))
        .transpose()?
        .unwrap_or(base);

    // The local commit and its parent, if given
    let local = if let Some(spec) = opts.local.as_deref() {
        let local = find_commit(git, spec)?;
        let parent = git
            .repo()
            .find_commit(local)?
            .parent_id(0)
            .map_err(|_| eyre!("'{spec}' has no parent commit"))?;
        Some((local, parent))
    } else {
        None
    };

    let (head_tree, base_tree) = match (local, &opts.tree, &opts.base_tree) {
        (Some((local, parent)), _, _) => (
            git.get_tree_oid_for_commit(local)?,
            git.get_tree_oid_for_commit(parent)?,
        ),
        (None, Some(tree), Some(base_tree)) => {
            (find_tree(git, tree)?, find_tree(git, base_tree)?)
        }
        _ => return Err(eyre!("--local or --tree and --base-tree required")),
    };

    let (head_tree, base_tree) = if opts.cherry_pick {
        pr_commits::cherry_pick(git, target, head_tree, base_tree)?
    } else {
        (head_tree, base_tree)
    };

    let commits = PullRequestCommits {
        head,
        base,
        target,
        head_tree,
        base_tree,
        may_update_base: !opts.fixed_base,
    };
    let plan = commits.plan(git)?;

    if opts.plan {
        let parents = |parents: &[Oid]| {
            parents.iter().map(Oid::to_string).collect::<Vec<_>>()
        };
        let head_parents = |parents: &[Parent]| {
            parents
                .iter()
                .map(|parent| match parent {
                    Parent::Commit(oid) => oid.to_string(),
                    Parent::NewBase => "new-base".to_string(),
                })
                .collect::<Vec<_>>()
        };
        let new_base_parents = plan.new_base_parents.as_deref().map(parents);
        let new_head_parents =
            plan.new_head_parents.as_deref().map(head_parents);

        if opts.json {
            writeln!(
                out,
                "{}",
                json!({
                    "new_base_parents": new_base_parents,
                    "new_head_parents": new_head_parents,
                    "merges": plan.merges,
                    "tree": head_tree.to_string(),
                    "base_tree": base_tree.to_string(),
                })
            )?;
        } else {
            if let Some(parents) = new_base_parents {
                writeln!(out, "new-base {}", parents.join(" "))?;
            }
            if let Some(parents) = new_head_parents {
                writeln!(out, "new-head {}", parents.join(" "))?;
            }
        }
        return Ok(());
    }

    if plan.new_head_parents.is_some() && opts.message.is_none() {
        return Err(PlumbingError {
            kind: "message-required",
            exit_code: 3,
            message: "a new head commit is needed, but no message was given \
                      (-m)"
                .to_string(),
        }
        .into());
    }

    let author_from = opts
        .author_from
        .as_deref()
        .map(|spec| find_commit(git, spec))
        .transpose()?;
    let base_message = opts.base_message.unwrap_or_else(|| {
        format!(
            "[𝘀𝗽𝗿] changes introduced through rebase\n\n\
             Created using spr {}\n\n[skip ci]",
            env!("CARGO_PKG_VERSION"),
        )
    });
    let head_message = opts.message.unwrap_or_default();

    let result = commits.create(
        git,
        &plan,
        CommitInfo {
            message: &base_message,
            author_from: author_from.or(local.map(|(_, parent)| parent)),
        },
        CommitInfo {
            message: &head_message,
            author_from: author_from.or(local.map(|(local, _)| local)),
        },
    )?;

    if opts.json {
        writeln!(
            out,
            "{}",
            json!({
                "head": result.head.to_string(),
                "base": result.base.to_string(),
                "head_created": result.head != head,
                "base_created": result.base != base,
            })
        )?;
    } else {
        writeln!(out, "{}", result.head)?;
        writeln!(out, "{}", result.base)?;
    }

    Ok(())
}

fn stack(opts: StackOptions, git: &Git, out: &mut dyn Write) -> Result<()> {
    let target = find_commit(git, &opts.target)?;
    let head = find_commit(git, &opts.commit)?;

    let mut entries = Vec::new();
    for oid in git.get_commit_oids_between(target, head)? {
        let commit = git.repo().find_commit(oid)?;
        if commit.parent_count() != 1 {
            return Err(PlumbingError {
                kind: "merge-commit",
                exit_code: 2,
                message: format!(
                    "{oid} is a merge commit, which spr can't handle"
                ),
            }
            .into());
        }
        let message = CommitMessage::parse(&String::from_utf8_lossy(
            commit.message_bytes(),
        ));
        entries.push((oid, commit.parent_id(0)?, message));
    }

    if opts.json {
        let entries: Vec<_> = entries
            .iter()
            .map(|(oid, parent, message)| {
                json!({
                    "commit": oid.to_string(),
                    "parent": parent.to_string(),
                    "title": message.title(),
                    "pull_request": message.get_trailer("Pull-request"),
                    "trailers": message.trailers(),
                })
            })
            .collect();
        writeln!(out, "{}", serde_json::Value::Array(entries))?;
    } else {
        for (oid, parent, message) in &entries {
            writeln!(
                out,
                "{oid} {parent} {}",
                message.get_trailer("Pull-request").unwrap_or("-")
            )?;
        }
    }

    Ok(())
}

fn land_check(
    opts: LandCheckOptions,
    git: &Git,
    out: &mut dyn Write,
) -> Result<()> {
    let target = find_commit(git, &opts.target)?;
    let pr_head = find_commit(git, &opts.pr_head)?;
    let local = find_commit(git, &opts.local)?;
    let since = match opts.since.as_deref() {
        Some(spec) => find_commit(git, spec)?,
        None => git
            .repo()
            .find_commit(local)?
            .parent_id(0)
            .map_err(|_| eyre!("'{}' has no parent commit", opts.local))?,
    };

    let conflict = |message: &str| PlumbingError {
        kind: "conflict",
        exit_code: 4,
        message: message.to_string(),
    };

    match land_check::land_check(git, target, pr_head, local, since)? {
        LandCheck::Ok(tree) => {
            if opts.json {
                writeln!(out, "{}", json!({ "tree": tree.to_string() }))?;
            } else {
                writeln!(out, "{tree}")?;
            }
            Ok(())
        }
        LandCheck::LocalConflict => Err(conflict(
            "the local commits can't be applied to the target without \
             conflicts",
        )
        .into()),
        LandCheck::MergeConflict => Err(conflict(
            "merging the Pull Request into the target has conflicts",
        )
        .into()),
        LandCheck::Mismatch {
            local_tree,
            merge_tree,
        } => Err(PlumbingError {
            kind: "mismatch",
            exit_code: 2,
            message: format!(
                "merging the Pull Request gives tree {merge_tree}, applying \
                 the local commits gives tree {local_tree}"
            ),
        }
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::TestRepo;
    use clap::Parser;

    #[derive(Debug, clap::Parser)]
    struct TestCli {
        #[clap(subcommand)]
        command: PlumbingCommand,
    }

    /// Run a plumbing command with the given arguments. Returns its output,
    /// or the error.
    fn plumbing(r: &TestRepo, args: &[&str]) -> Result<String> {
        let cli = TestCli::try_parse_from(
            std::iter::once("plumbing").chain(args.iter().copied()),
        )?;
        let mut out = Vec::new();
        run(cli.command, &r.git, &mut out)?;
        Ok(String::from_utf8(out)?)
    }

    fn lines(output: &str) -> Vec<&str> {
        output.lines().collect()
    }

    #[test]
    fn test_commit_pr_new_pull_request() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let local = r.commit("b", &[m1]);

        let output = plumbing(
            &r,
            &[
                "commit-pr",
                "--base",
                &m1.to_string(),
                "--target",
                &m1.to_string(),
                "--local",
                &local.to_string(),
                "-m",
                "initial version",
            ],
        )
        .unwrap();

        let output = lines(&output);
        let head: Oid = output[0].parse().unwrap();
        assert_eq!(output[1], m1.to_string());
        assert_eq!(r.parents_of(head), vec![m1]);
        assert_eq!(r.tree_of(head), r.tree("b"));
        assert_eq!(r.message_of(head), "initial version\n");
    }

    #[test]
    fn test_commit_pr_nothing_to_do() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let head = r.commit("b", &[m1]);

        // Tree syntax: the head tree is the tree of the current head
        let output = plumbing(
            &r,
            &[
                "commit-pr",
                "--head",
                &head.to_string(),
                "--base",
                &m1.to_string(),
                "--target",
                &m1.to_string(),
                "--tree",
                &format!("{head}:"),
                "--base-tree",
                &format!("{m1}:"),
            ],
        )
        .unwrap();

        // No message needed, as no commit is created
        assert_eq!(lines(&output), vec![head.to_string(), m1.to_string()]);
    }

    #[test]
    fn test_commit_pr_plan() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let local_parent = r.commit("a", &[m1]);
        let local = r.commit("b", &[local_parent]);
        let args = [
            "commit-pr",
            "--base",
            &m1.to_string(),
            "--target",
            &m1.to_string(),
            "--local",
            &local.to_string(),
            "--plan",
        ];

        let output = plumbing(&r, &args).unwrap();
        assert_eq!(
            lines(&output),
            vec![format!("new-base {m1}"), "new-head new-base".to_string()]
        );

        let output = plumbing(&r, &[&args[..], &["--json"]].concat()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["new_base_parents"], json!([m1.to_string()]));
        assert_eq!(json["new_head_parents"], json!(["new-base"]));
        assert_eq!(json["merges"], json!(false));
    }

    #[test]
    fn test_commit_pr_message_required() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let local = r.commit("b", &[m1]);

        let error = plumbing(
            &r,
            &[
                "commit-pr",
                "--base",
                &m1.to_string(),
                "--target",
                &m1.to_string(),
                "--local",
                &local.to_string(),
            ],
        )
        .unwrap_err();

        assert_eq!(classify(&error), ("message-required", 3));
    }

    #[test]
    fn test_commit_pr_fixed_base_outdated() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let parent_head = r.commit("a", &[m1]);
        let local_parent = r.commit("amended a", &[m1]);
        let local = r.commit("b", &[local_parent]);

        let error = plumbing(
            &r,
            &[
                "commit-pr",
                "--base",
                &parent_head.to_string(),
                "--target",
                &m1.to_string(),
                "--local",
                &local.to_string(),
                "--fixed-base",
                "-m",
                "x",
            ],
        )
        .unwrap_err();

        assert_eq!(classify(&error), ("base-outdated", 2));
        assert_eq!(error_json(&error)["error"]["kind"], json!("base-outdated"));
    }

    #[test]
    fn test_commit_pr_cherry_pick() {
        let r = TestRepo::new();
        let m1 = r.commit_tree(r.tree_with(&[("x", "1")]), &[]);
        let m2 = r.commit_tree(r.tree_with(&[("x", "2")]), &[m1]);
        let a = r.commit_tree(r.tree_with(&[("x", "1"), ("a", "a")]), &[m1]);
        let local = r.commit_tree(
            r.tree_with(&[("x", "1"), ("a", "a"), ("b", "b")]),
            &[a],
        );

        let output = plumbing(
            &r,
            &[
                "commit-pr",
                "--base",
                &m2.to_string(),
                "--target",
                &m2.to_string(),
                "--local",
                &local.to_string(),
                "--cherry-pick",
                "--fixed-base",
                "-m",
                "cherry-picked",
            ],
        )
        .unwrap();

        let head: Oid = lines(&output)[0].parse().unwrap();
        assert_eq!(r.parents_of(head), vec![m2]);
        assert_eq!(r.tree_of(head), r.tree_with(&[("x", "2"), ("b", "b")]));
    }

    #[test]
    fn test_commit_pr_cherry_pick_conflict() {
        let r = TestRepo::new();
        let m1 = r.commit_tree(r.tree_with(&[("x", "1")]), &[]);
        let m2 = r.commit_tree(r.tree_with(&[("x", "2")]), &[m1]);
        let local = r.commit_tree(r.tree_with(&[("x", "b")]), &[m1]);

        let error = plumbing(
            &r,
            &[
                "commit-pr",
                "--base",
                &m2.to_string(),
                "--target",
                &m2.to_string(),
                "--local",
                &local.to_string(),
                "--cherry-pick",
                "-m",
                "x",
            ],
        )
        .unwrap_err();

        assert_eq!(classify(&error), ("conflict", 4));
    }

    #[test]
    fn test_commit_pr_json_and_author() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);

        let output = plumbing(
            &r,
            &[
                "commit-pr",
                "--base",
                &m1.to_string(),
                "--target",
                &m1.to_string(),
                "--tree",
                &r.tree("b").to_string(),
                "--base-tree",
                &format!("{m1}:"),
                "-m",
                "x",
                "--json",
            ],
        )
        .unwrap();

        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["base"], json!(m1.to_string()));
        assert_eq!(json["head_created"], json!(true));
        assert_eq!(json["base_created"], json!(false));

        // Without --local or --author-from, the author is the current user
        let head: Oid = json["head"].as_str().unwrap().parse().unwrap();
        let head = r.git.repo().find_commit(head).unwrap();
        assert_eq!(head.author().name(), Ok("Test"));
    }

    /// A commit with the given message and a tree with the message as
    /// content
    fn commit_with_message(r: &TestRepo, message: &str, parent: Oid) -> Oid {
        let repo = r.git.repo();
        let tree = repo.find_tree(r.tree(message)).unwrap();
        let signature = repo.signature().unwrap();
        repo.commit(
            None,
            &signature,
            &signature,
            message,
            &tree,
            &[&repo.find_commit(parent).unwrap()],
        )
        .unwrap()
    }

    #[test]
    fn test_stack() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let a = commit_with_message(
            &r,
            "A\n\nPull-request: https://github.com/o/r/pull/1\n",
            m1,
        );
        let b = commit_with_message(&r, "B\n\nNo pull request yet.\n", a);

        let output = plumbing(
            &r,
            &["stack", "--target", &m1.to_string(), &b.to_string()],
        )
        .unwrap();
        assert_eq!(
            lines(&output),
            vec![
                format!("{a} {m1} https://github.com/o/r/pull/1"),
                format!("{b} {a} -"),
            ]
        );

        let output = plumbing(
            &r,
            &[
                "stack",
                "--target",
                &m1.to_string(),
                &b.to_string(),
                "--json",
            ],
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json[0]["title"], json!("A"));
        assert_eq!(
            json[0]["pull_request"],
            json!("https://github.com/o/r/pull/1")
        );
        assert_eq!(
            json[0]["trailers"],
            json!([["Pull-request", "https://github.com/o/r/pull/1"]])
        );
        assert_eq!(json[1]["pull_request"], json!(null));
    }

    #[test]
    fn test_stack_empty() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);

        let output = plumbing(
            &r,
            &["stack", "--target", &m1.to_string(), &m1.to_string()],
        )
        .unwrap();
        assert_eq!(output, "");
    }

    #[test]
    fn test_stack_merge_commit() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let a = r.commit("a", &[m1]);
        let b = r.commit("b", &[m1]);
        let merge = r.commit("merge", &[a, b]);

        let error = plumbing(
            &r,
            &["stack", "--target", &m1.to_string(), &merge.to_string()],
        )
        .unwrap_err();
        assert_eq!(classify(&error), ("merge-commit", 2));
    }

    #[test]
    fn test_land_check() {
        let r = TestRepo::new();
        let m1 = r.commit_tree(r.tree_with(&[("x", "1")]), &[]);
        let m2 = r.commit_tree(r.tree_with(&[("x", "2")]), &[m1]);
        let b = r.commit_tree(r.tree_with(&[("x", "1"), ("y", "b")]), &[m1]);
        let pr_head = r.commit_tree(r.tree_of(b), &[m1]);
        let args = |pr_head: Oid| {
            vec![
                "land-check".to_string(),
                "--target".to_string(),
                m2.to_string(),
                "--pr-head".to_string(),
                pr_head.to_string(),
                "--local".to_string(),
                b.to_string(),
            ]
        };
        let run = |args: Vec<String>| {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            plumbing(&r, &args)
        };

        let output = run(args(pr_head)).unwrap();
        assert_eq!(
            lines(&output),
            vec![r.tree_with(&[("x", "2"), ("y", "b")]).to_string()]
        );

        // The Pull Request doesn't reflect the local commit
        let outdated =
            r.commit_tree(r.tree_with(&[("x", "1"), ("y", "old")]), &[m1]);
        let error = run(args(outdated)).unwrap_err();
        assert_eq!(classify(&error), ("mismatch", 2));
    }
}
