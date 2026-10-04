//! Working out the changes others made to a Pull Request, and applying them
//! to the local commit (`spr pull`).
//!
//! The changes are what changed on the Pull Request since it had the head
//! spr recorded for the local commit (the expected head, see
//! `Git::get_expected_head`). They're applied to the local commit with a
//! three-way merge: base = the expected head, ours = the local commit's
//! tree, theirs = the Pull Request's current head.

use color_eyre::eyre::Result;
use git2::Oid;

use crate::{
    git::Git,
    pr_commits::{merge_trees, remote_changes_baseline},
};

/// What pulling the changes of a Pull Request into the local commit gives
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullOutcome {
    /// The local commit contains all the changes of the Pull Request already
    NothingToPull,
    /// The changes apply cleanly: the local commit's new tree
    Merged { tree: Oid },
    /// The changes conflict with the local commit. `changes` is a commit
    /// whose diff is exactly the changes, for cherry-picking.
    Conflict { changes: Oid },
    /// The Pull Request's history since the expected head isn't just plain
    /// commits on top of it (it has merge commits, or was rewritten), so
    /// it's not clear which changes were made by others. Pull anyway with
    /// `force`.
    NotLinear,
}

/// Work out what pulling the changes of a Pull Request into a local commit
/// gives. `local_tree` is the local commit's tree, `target` the
/// target-branch commit the local branch is based on, `head` the Pull
/// Request's current head, and `expected_head` the head recorded for the
/// local commit.
///
/// Without `force`, only pulls if the Pull Request's history since the
/// expected head is a chain of plain commits. With `force`, it pulls anyway,
/// taking into account the target branch changes the Pull Request picked up
/// (see `remote_changes_baseline`).
///
/// On conflicts, creates a commit with the changes, with the given message
/// (no refs are changed).
pub fn pull_changes(
    git: &Git,
    local_tree: Oid,
    target: Oid,
    head: Oid,
    expected_head: Oid,
    force: bool,
    changes_message: &str,
) -> Result<PullOutcome> {
    if head == expected_head {
        return Ok(PullOutcome::NothingToPull);
    }
    let tree = |commit: Oid| git.get_tree_oid_for_commit(commit);
    let head_tree = tree(head)?;

    // The local commit may contain the changes already, e.g. if the Pull
    // Request was only rebased or had the target branch merged in, or if the
    // changes were applied locally already.
    let baseline = remote_changes_baseline(git, head, target, expected_head)?;
    if let Some(baseline) = baseline
        && merge_trees(git, baseline, local_tree, head_tree)?
            == Some(local_tree)
    {
        return Ok(PullOutcome::NothingToPull);
    }

    // The base of the three-way merge: a commit with its tree
    let base = if is_linear(git, expected_head, head)? {
        expected_head
    } else if !force {
        return Ok(PullOutcome::NotLinear);
    } else if let Some(baseline) = baseline {
        git.repo().commit(
            None,
            &signature(git)?,
            &signature(git)?,
            "Baseline",
            &git.repo().find_tree(baseline)?,
            &[],
        )?
    } else {
        expected_head
    };

    match merge_trees(git, tree(base)?, local_tree, head_tree)? {
        Some(merged) if merged == local_tree => Ok(PullOutcome::NothingToPull),
        Some(merged) => Ok(PullOutcome::Merged { tree: merged }),
        None => {
            let changes = git.repo().commit(
                None,
                &signature(git)?,
                &signature(git)?,
                changes_message,
                &git.repo().find_tree(head_tree)?,
                &[&git.repo().find_commit(base)?],
            )?;
            Ok(PullOutcome::Conflict { changes })
        }
    }
}

/// Whether `head` is `ancestor` with a chain of plain (non-merge) commits on
/// top
fn is_linear(git: &Git, ancestor: Oid, head: Oid) -> Result<bool> {
    if !git.is_ancestor(ancestor, head)? {
        return Ok(false);
    }
    let repo = git.repo();
    let mut walk = repo.revwalk()?;
    walk.push(head)?;
    walk.hide(ancestor)?;
    for oid in walk {
        if repo.find_commit(oid?)?.parent_count() != 1 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The signature for commits spr creates: the current user, or a generic
/// one if none is configured
fn signature(git: &Git) -> Result<git2::Signature<'static>> {
    Ok(match git.repo().signature() {
        Ok(signature) => signature.to_owned(),
        Err(_) => git2::Signature::now("spr", "spr@localhost")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::TestRepo;

    const FILE: &str = "a\nb\nc\nd\ne\nf\ng\n";

    /// Replace line `n` (0-based) of `FILE`-like content
    fn edit(content: &str, n: usize, line: &str) -> String {
        let mut lines: Vec<_> = content.lines().collect();
        lines[n] = line;
        lines.join("\n") + "\n"
    }

    /// A repository with master commit m1 and the expected head l, whose
    /// change is the file `pr` (on top of m1's `file`)
    fn setup() -> (TestRepo, Oid, Oid) {
        let r = TestRepo::new();
        let m1 = r.commit_tree(r.tree_with(&[("file", FILE)]), &[]);
        let l = r
            .commit_tree(r.tree_with(&[("file", FILE), ("pr", "pr\n")]), &[m1]);
        (r, m1, l)
    }

    fn pull(
        r: &TestRepo,
        local: &[(&str, &str)],
        target: Oid,
        head: Oid,
        expected_head: Oid,
        force: bool,
    ) -> PullOutcome {
        pull_changes(
            &r.git,
            r.tree_with(local),
            target,
            head,
            expected_head,
            force,
            "changes",
        )
        .unwrap()
    }

    #[test]
    fn test_unchanged() {
        let (r, m1, l) = setup();
        let local = [("file", FILE), ("pr", "pr, changed locally\n")];
        assert_eq!(
            pull(&r, &local, m1, l, l, false),
            PullOutcome::NothingToPull
        );
    }

    #[test]
    fn test_plain_commit_on_top() {
        // Somebody fixed a typo on the Pull Request; the local commit has
        // unpushed changes of its own
        let (r, m1, l) = setup();
        let fixed = edit(FILE, 1, "B");
        let head = r.commit_tree(
            r.tree_with(&[("file", &fixed), ("pr", "pr\n")]),
            &[l],
        );
        let local = edit(FILE, 5, "F");

        let outcome =
            pull(&r, &[("file", &local), ("pr", "pr\n")], m1, head, l, false);

        let expected = edit(&local, 1, "B");
        assert_eq!(
            outcome,
            PullOutcome::Merged {
                tree: r.tree_with(&[("file", &expected), ("pr", "pr\n")])
            }
        );
    }

    #[test]
    fn test_changes_applied_locally_already() {
        let (r, m1, l) = setup();
        let fixed = edit(FILE, 1, "B");
        let head = r.commit_tree(
            r.tree_with(&[("file", &fixed), ("pr", "pr\n")]),
            &[l],
        );
        let local = [("file", fixed.as_str()), ("pr", "pr\n")];
        assert_eq!(
            pull(&r, &local, m1, head, l, false),
            PullOutcome::NothingToPull
        );
    }

    #[test]
    fn test_conflict() {
        let (r, m1, l) = setup();
        let theirs = edit(FILE, 1, "theirs");
        let head = r.commit_tree(
            r.tree_with(&[("file", &theirs), ("pr", "pr\n")]),
            &[l],
        );
        let ours = edit(FILE, 1, "ours");

        let outcome =
            pull(&r, &[("file", &ours), ("pr", "pr\n")], m1, head, l, false);

        // The commit with the changes: its diff is exactly what changed on
        // the Pull Request
        let PullOutcome::Conflict { changes } = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(r.parents_of(changes), vec![l]);
        assert_eq!(r.tree_of(changes), r.tree_of(head));
        assert_eq!(r.message_of(changes), "changes");
    }

    #[test]
    fn test_target_merged_in_and_contained_locally() {
        // GitHub's "Update branch", and the local commit was rebased onto
        // the same master commit
        let (r, m1, l) = setup();
        let m2_file = edit(FILE, 6, "G");
        let m2 = r.commit_tree(r.tree_with(&[("file", &m2_file)]), &[m1]);
        let head = r.commit_tree(
            r.tree_with(&[("file", &m2_file), ("pr", "pr\n")]),
            &[l, m2],
        );
        let local = [("file", m2_file.as_str()), ("pr", "pr\n")];
        assert_eq!(
            pull(&r, &local, m2, head, l, false),
            PullOutcome::NothingToPull
        );
    }

    #[test]
    fn test_merges_need_force() {
        // "Update branch" and a typo fix on top. The local commit is still
        // based on m1.
        let (r, m1, l) = setup();
        let m2_file = edit(FILE, 6, "G");
        let m2 = r.commit_tree(r.tree_with(&[("file", &m2_file)]), &[m1]);
        let merged = r.commit_tree(
            r.tree_with(&[("file", &m2_file), ("pr", "pr\n")]),
            &[l, m2],
        );
        let fixed = edit(&m2_file, 1, "B");
        let head = r.commit_tree(
            r.tree_with(&[("file", &fixed), ("pr", "pr\n")]),
            &[merged],
        );
        let local = [("file", FILE), ("pr", "pr\n")];

        assert_eq!(
            pull(&r, &local, m1, head, l, false),
            PullOutcome::NotLinear
        );

        // With force, the master changes come along, too
        assert_eq!(
            pull(&r, &local, m1, head, l, true),
            PullOutcome::Merged {
                tree: r.tree_with(&[("file", &fixed), ("pr", "pr\n")])
            }
        );

        // After rebasing the local commit onto m2 (and changing it further),
        // only the typo fix comes in
        let local = edit(&m2_file, 3, "D");
        assert_eq!(
            pull(&r, &[("file", &local), ("pr", "pr\n")], m2, head, l, true),
            PullOutcome::Merged {
                tree: r.tree_with(&[
                    ("file", &edit(&local, 1, "B")),
                    ("pr", "pr\n")
                ])
            }
        );
    }

    #[test]
    fn test_rewritten_history_needs_force() {
        // Somebody force-pushed a changed version of the Pull Request
        let (r, m1, l) = setup();
        let head = r.commit_tree(
            r.tree_with(&[("file", &edit(FILE, 1, "B")), ("pr", "pr\n")]),
            &[m1],
        );
        let local = [("file", FILE), ("pr", "pr\n")];

        assert_eq!(
            pull(&r, &local, m1, head, l, false),
            PullOutcome::NotLinear
        );
        assert_eq!(
            pull(&r, &local, m1, head, l, true),
            PullOutcome::Merged {
                tree: r.tree_of(head)
            }
        );
    }

    #[test]
    fn test_conflict_with_force_uses_baseline() {
        // With force, the changes commit is based on the baseline: the
        // expected head with the master changes the Pull Request picked up
        let (r, m1, l) = setup();
        let m2_file = edit(FILE, 6, "G");
        let m2 = r.commit_tree(r.tree_with(&[("file", &m2_file)]), &[m1]);
        let merged = r.commit_tree(
            r.tree_with(&[("file", &m2_file), ("pr", "pr\n")]),
            &[l, m2],
        );
        let head = r.commit_tree(
            r.tree_with(&[
                ("file", &edit(&m2_file, 1, "theirs")),
                ("pr", "pr\n"),
            ]),
            &[merged],
        );
        let local = edit(&m2_file, 1, "ours");

        let outcome =
            pull(&r, &[("file", &local), ("pr", "pr\n")], m2, head, l, true);

        let PullOutcome::Conflict { changes } = outcome else {
            panic!("{outcome:?}");
        };
        let base = r.parents_of(changes)[0];
        assert_eq!(
            r.tree_of(base),
            r.tree_with(&[("file", &m2_file), ("pr", "pr\n")])
        );
        assert_eq!(r.tree_of(changes), r.tree_of(head));
    }
}
