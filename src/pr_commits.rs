//! Constructing the commits of a Pull Request.
//!
//! spr projects a local commit onto a Pull Request: the Pull Request branch
//! (the "head") gets a new commit with the tree of the local commit, and the
//! Pull Request's base must have the tree of the local commit's parent. This
//! module works out which commits that takes, and creates them. It only deals
//! with Git objects: it doesn't touch any refs and doesn't talk to GitHub.
//!
//! The same operation serves all situations, fed with different inputs:
//!
//! - A commit directly based on the target branch (or cherry-picked onto it):
//!   the base is the target commit, and base commits can't be added.
//! - A commit with a synthetic base branch: the base is the tip of the base
//!   branch, and base commits may be added.
//! - A commit stacked on the Pull Request of its parent commit: the base is
//!   that Pull Request's head, and base commits can't be added. If the base
//!   doesn't reflect the parent commit, the operation fails.
//! - A new Pull Request: the head is the same commit as the base.

use color_eyre::eyre::Result;
use git2::Oid;

use crate::git::Git;

/// The inputs for updating the commits of a Pull Request
#[derive(Debug, Clone)]
pub struct PullRequestCommits {
    /// The current head of the Pull Request branch. For a new Pull Request,
    /// this is the same as `base`.
    pub head: Oid,
    /// The commit the Pull Request is currently based on
    pub base: Oid,
    /// The commit on the target branch that the local commit is based on
    pub target: Oid,
    /// The tree the head of the Pull Request should have
    pub head_tree: Oid,
    /// The tree the base of the Pull Request should have
    pub base_tree: Oid,
    /// Whether new commits may be added to the base
    pub may_update_base: bool,
}

/// A parent of the new head commit
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parent {
    /// An existing commit
    Commit(Oid),
    /// The new base commit
    NewBase,
}

/// What it takes to update the commits of a Pull Request
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The parents of a new base commit, if the base needs updating
    pub new_base_parents: Option<Vec<Oid>>,
    /// The parents of a new head commit, if the head needs updating
    pub new_head_parents: Option<Vec<Parent>>,
    /// Whether the new head commit merges something into the Pull Request
    /// branch, i.e. the local commit was rebased
    pub merges: bool,
}

impl Plan {
    /// Whether nothing needs to be done
    pub fn is_empty(&self) -> bool {
        self.new_base_parents.is_none() && self.new_head_parents.is_none()
    }
}

/// The base of the Pull Request doesn't reflect the parent of the local
/// commit, and we may not add commits to it.
#[derive(Debug)]
pub struct BaseOutdated;

impl std::fmt::Display for BaseOutdated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the base of the Pull Request is not up to date")
    }
}

impl std::error::Error for BaseOutdated {}

/// Applying a change caused conflicts.
#[derive(Debug)]
pub struct Conflict;

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("applying the change causes conflicts")
    }
}

impl std::error::Error for Conflict {}

/// The trees for cherry-picking a change onto `target`: the change from
/// `base_tree` to `head_tree` is applied to the tree of `target`. Returns
/// the new head tree and base tree (the tree of `target`).
///
/// Fails with `Conflict` if the change can't be applied cleanly.
pub fn cherry_pick(
    git: &Git,
    target: Oid,
    head_tree: Oid,
    base_tree: Oid,
) -> Result<(Oid, Oid)> {
    let repo = git.repo();
    let target_tree = repo.find_commit(target)?.tree()?;
    let mut index = repo.merge_trees(
        &repo.find_tree(base_tree)?,
        &target_tree,
        &repo.find_tree(head_tree)?,
        None,
    )?;
    if index.has_conflicts() {
        return Err(Conflict.into());
    }

    Ok((index.write_tree_to(repo)?, target_tree.id()))
}

/// The resulting head and base of the Pull Request
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Commits {
    pub head: Oid,
    pub base: Oid,
}

/// A commit to be created: its message, and the commit whose author it
/// credits, if any (see `Git::create_derived_commit`)
#[derive(Debug, Clone, Copy)]
pub struct CommitInfo<'a> {
    pub message: &'a str,
    pub author_from: Option<Oid>,
}

impl PullRequestCommits {
    /// Work out which commits are needed. This doesn't create any commits.
    ///
    /// Fails with `BaseOutdated` if the base needs updating, but
    /// `may_update_base` is false.
    pub fn plan(&self, git: &Git) -> Result<Plan> {
        // The base must have the right tree, and contain the target commit.
        let base_has_target = git.is_ancestor(self.target, self.base)?;
        let base_is_up_to_date = base_has_target
            && git.get_tree_oid_for_commit(self.base)? == self.base_tree;

        let new_base_parents = if base_is_up_to_date {
            None
        } else if !self.may_update_base {
            return Err(BaseOutdated.into());
        } else {
            // The new base commit is based on the current base, and merges
            // in the target commit, if necessary.
            let mut parents = vec![self.base];
            if !base_has_target {
                parents.push(self.target);
            }
            Some(parents)
        };

        // The head must contain the (new) base.
        let merge = if new_base_parents.is_some() {
            Some(Parent::NewBase)
        } else if !git.is_ancestor(self.base, self.head)? {
            Some(Parent::Commit(self.base))
        } else {
            None
        };

        let new_head_parents = if let Some(merge) = merge {
            // The first parent is the current head, unless the head is
            // contained in what we merge anyway (as for a new Pull Request,
            // where the head starts off at the base).
            let head_in_merge = match merge {
                Parent::Commit(oid) => git.is_ancestor(self.head, oid)?,
                Parent::NewBase => {
                    let mut result = false;
                    for &oid in new_base_parents.iter().flatten() {
                        result = result || git.is_ancestor(self.head, oid)?;
                    }
                    result
                }
            };
            if head_in_merge {
                Some(vec![merge])
            } else {
                Some(vec![Parent::Commit(self.head), merge])
            }
        } else if git.get_tree_oid_for_commit(self.head)? != self.head_tree {
            Some(vec![Parent::Commit(self.head)])
        } else {
            None
        };

        let merges = new_head_parents
            .as_ref()
            .is_some_and(|parents| parents.len() > 1);

        Ok(Plan {
            new_base_parents,
            new_head_parents,
            merges,
        })
    }

    /// Create the commits of a plan. Returns the resulting head and base.
    pub fn create(
        &self,
        git: &Git,
        plan: &Plan,
        base_commit: CommitInfo,
        head_commit: CommitInfo,
    ) -> Result<Commits> {
        let base = if let Some(parents) = &plan.new_base_parents {
            git.create_derived_commit(
                base_commit.author_from,
                base_commit.message,
                self.base_tree,
                parents,
            )?
        } else {
            self.base
        };

        let head = if let Some(parents) = &plan.new_head_parents {
            let parents: Vec<Oid> = parents
                .iter()
                .map(|parent| match parent {
                    Parent::Commit(oid) => *oid,
                    Parent::NewBase => base,
                })
                .collect();
            git.create_derived_commit(
                head_commit.author_from,
                head_commit.message,
                self.head_tree,
                &parents,
            )?
        } else {
            self.head
        };

        Ok(Commits { head, base })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_utils::TestRepo;

    /// Plan and create, with messages "base" and "head"
    fn run(
        r: &TestRepo,
        input: &PullRequestCommits,
    ) -> Result<(Plan, Commits)> {
        let plan = input.plan(&r.git)?;
        let commits = input.create(
            &r.git,
            &plan,
            CommitInfo {
                message: "base",
                author_from: Some(input.base),
            },
            CommitInfo {
                message: "head",
                author_from: Some(input.head),
            },
        )?;
        Ok((plan, commits))
    }

    // The following tests use a master branch with commits m1 and m2, and a
    // local branch with commits a (the parent) and b (the local commit we
    // create a Pull Request for).

    #[test]
    fn test_new_pull_request_on_target() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);

        let (plan, commits) = run(
            &r,
            &PullRequestCommits {
                head: m1,
                base: m1,
                target: m1,
                head_tree: r.tree("b"),
                base_tree: r.tree("m1"),
                may_update_base: false,
            },
        )
        .unwrap();

        assert!(!plan.merges);
        assert_eq!(commits.base, m1);
        assert_eq!(r.parents_of(commits.head), vec![m1]);
        assert_eq!(r.tree_of(commits.head), r.tree("b"));
        assert_eq!(r.message_of(commits.head), "head\n");
    }

    #[test]
    fn test_amended_pull_request_on_target() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let head = r.commit("b", &[m1]);

        let (plan, commits) = run(
            &r,
            &PullRequestCommits {
                head,
                base: m1,
                target: m1,
                head_tree: r.tree("b amended"),
                base_tree: r.tree("m1"),
                may_update_base: false,
            },
        )
        .unwrap();

        assert!(!plan.merges);
        assert_eq!(commits.base, m1);
        assert_eq!(r.parents_of(commits.head), vec![head]);
        assert_eq!(r.tree_of(commits.head), r.tree("b amended"));
    }

    #[test]
    fn test_nothing_to_do() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let head = r.commit("b", &[m1]);

        let input = PullRequestCommits {
            head,
            base: m1,
            target: m1,
            head_tree: r.tree("b"),
            base_tree: r.tree("m1"),
            may_update_base: false,
        };
        let plan = input.plan(&r.git).unwrap();

        assert!(plan.is_empty());
        let (_, commits) = run(&r, &input).unwrap();
        assert_eq!(commits, Commits { head, base: m1 });
    }

    #[test]
    fn test_rebased_onto_newer_target() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let m2 = r.commit("m2", &[m1]);
        let head = r.commit("b", &[m1]);

        // The local commit is now based on m2. With base = target, the target
        // gets merged into the head directly.
        let (plan, commits) = run(
            &r,
            &PullRequestCommits {
                head,
                base: m2,
                target: m2,
                head_tree: r.tree("b on m2"),
                base_tree: r.tree("m2"),
                may_update_base: false,
            },
        )
        .unwrap();

        assert!(plan.merges);
        assert_eq!(plan.new_base_parents, None);
        assert_eq!(commits.base, m2);
        assert_eq!(r.parents_of(commits.head), vec![head, m2]);
        assert_eq!(r.tree_of(commits.head), r.tree("b on m2"));
    }

    #[test]
    fn test_rebased_without_content_change() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let head = r.commit("b", &[m1]);
        // m2 has the same tree as m1 (e.g. a change that was reverted)
        let m2 = r.commit("m1", &[m1]);

        let (plan, commits) = run(
            &r,
            &PullRequestCommits {
                head,
                base: m2,
                target: m2,
                head_tree: r.tree("b"),
                base_tree: r.tree("m1"),
                may_update_base: false,
            },
        )
        .unwrap();

        // The head tree didn't change, but the target must be merged in.
        assert!(plan.merges);
        assert_eq!(r.parents_of(commits.head), vec![head, m2]);
    }

    #[test]
    fn test_new_pull_request_with_base_branch() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);

        let (plan, commits) = run(
            &r,
            &PullRequestCommits {
                head: m1,
                base: m1,
                target: m1,
                head_tree: r.tree("b"),
                base_tree: r.tree("a"),
                may_update_base: true,
            },
        )
        .unwrap();

        assert_eq!(plan.new_base_parents, Some(vec![m1]));
        assert_eq!(r.parents_of(commits.base), vec![m1]);
        assert_eq!(r.tree_of(commits.base), r.tree("a"));
        assert_eq!(r.message_of(commits.base), "base\n");
        // The head starts off at the base; it's not merged in separately.
        assert_eq!(r.parents_of(commits.head), vec![commits.base]);
        assert_eq!(r.tree_of(commits.head), r.tree("b"));
    }

    #[test]
    fn test_base_branch_parent_amended() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let base = r.commit("a", &[m1]);
        let head = r.commit("b", &[base]);

        let (plan, commits) = run(
            &r,
            &PullRequestCommits {
                head,
                base,
                target: m1,
                head_tree: r.tree("b on amended a"),
                base_tree: r.tree("amended a"),
                may_update_base: true,
            },
        )
        .unwrap();

        assert!(plan.merges);
        assert_eq!(r.parents_of(commits.base), vec![base]);
        assert_eq!(r.tree_of(commits.base), r.tree("amended a"));
        assert_eq!(r.parents_of(commits.head), vec![head, commits.base]);
    }

    #[test]
    fn test_base_branch_rebased_onto_newer_target() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let m2 = r.commit("m2", &[m1]);
        let base = r.commit("a", &[m1]);
        let head = r.commit("b", &[base]);

        let (_, commits) = run(
            &r,
            &PullRequestCommits {
                head,
                base,
                target: m2,
                head_tree: r.tree("b on m2"),
                base_tree: r.tree("a on m2"),
                may_update_base: true,
            },
        )
        .unwrap();

        // The new base commit merges in the new target.
        assert_eq!(r.parents_of(commits.base), vec![base, m2]);
        assert_eq!(r.parents_of(commits.head), vec![head, commits.base]);
    }

    #[test]
    fn test_base_branch_up_to_date() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let base = r.commit("a", &[m1]);
        let head = r.commit("b", &[base]);

        let (plan, commits) = run(
            &r,
            &PullRequestCommits {
                head,
                base,
                target: m1,
                head_tree: r.tree("b amended"),
                base_tree: r.tree("a"),
                may_update_base: true,
            },
        )
        .unwrap();

        assert_eq!(plan.new_base_parents, None);
        assert_eq!(commits.base, base);
        assert_eq!(r.parents_of(commits.head), vec![head]);
    }

    #[test]
    fn test_new_pull_request_stacked_on_parent_pull_request() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let parent_head = r.commit("a", &[m1]);

        let (plan, commits) = run(
            &r,
            &PullRequestCommits {
                head: parent_head,
                base: parent_head,
                target: m1,
                head_tree: r.tree("b"),
                base_tree: r.tree("a"),
                may_update_base: false,
            },
        )
        .unwrap();

        assert!(!plan.merges);
        assert_eq!(commits.base, parent_head);
        assert_eq!(r.parents_of(commits.head), vec![parent_head]);
    }

    #[test]
    fn test_stacked_parent_pull_request_updated() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let parent_head = r.commit("a", &[m1]);
        let head = r.commit("b", &[parent_head]);
        let new_parent_head = r.commit("amended a", &[parent_head]);

        let (plan, commits) = run(
            &r,
            &PullRequestCommits {
                head,
                base: new_parent_head,
                target: m1,
                head_tree: r.tree("b on amended a"),
                base_tree: r.tree("amended a"),
                may_update_base: false,
            },
        )
        .unwrap();

        assert!(plan.merges);
        assert_eq!(commits.base, new_parent_head);
        assert_eq!(r.parents_of(commits.head), vec![head, new_parent_head]);
    }

    #[test]
    fn test_stacked_parent_pull_request_outdated() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let m2 = r.commit("m2", &[m1]);
        let parent_head = r.commit("a", &[m1]);
        let head = r.commit("b", &[parent_head]);

        let plan = |base_tree: &str, target: Oid| {
            PullRequestCommits {
                head,
                base: parent_head,
                target,
                head_tree: r.tree("b"),
                base_tree: r.tree(base_tree),
                may_update_base: false,
            }
            .plan(&r.git)
        };

        // The parent commit was amended, but its Pull Request not updated
        let error = plan("amended a", m1).unwrap_err();
        assert!(error.downcast_ref::<BaseOutdated>().is_some());

        // The parent commit was rebased, but its Pull Request not updated
        let error = plan("a", m2).unwrap_err();
        assert!(error.downcast_ref::<BaseOutdated>().is_some());

        // Up to date
        assert!(plan("a", m1).is_ok());
    }

    #[test]
    fn test_cherry_pick() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let base = r.commit("a", &[m1]);
        let head = r.commit("b", &[base]);

        // With --cherry-pick, the Pull Request is based on the target, and
        // the head tree is the result of cherry-picking the local commit onto
        // it. The old base branch is left behind.
        let (plan, commits) = run(
            &r,
            &PullRequestCommits {
                head,
                base: m1,
                target: m1,
                head_tree: r.tree("b cherry-picked onto m1"),
                base_tree: r.tree("m1"),
                may_update_base: false,
            },
        )
        .unwrap();

        assert!(!plan.merges);
        assert_eq!(commits.base, m1);
        assert_eq!(r.parents_of(commits.head), vec![head]);
        assert_eq!(r.tree_of(commits.head), r.tree("b cherry-picked onto m1"));
    }

    #[test]
    fn test_base_branch_obsolete_after_parent_landed() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let base = r.commit("a", &[m1]);
        let head = r.commit("b", &[base]);
        // a was squash-merged into master, and the local branch rebased
        let m2 = r.commit("a", &[m1]);

        let (plan, commits) = run(
            &r,
            &PullRequestCommits {
                head,
                base: m2,
                target: m2,
                head_tree: r.tree("b"),
                base_tree: r.tree("a"),
                may_update_base: false,
            },
        )
        .unwrap();

        // The new master commit is merged into the head directly; the base
        // branch isn't needed anymore.
        assert!(plan.merges);
        assert_eq!(plan.new_base_parents, None);
        assert_eq!(commits.base, m2);
        assert_eq!(r.parents_of(commits.head), vec![head, m2]);
    }

    #[test]
    fn test_create_uses_author_of_given_commit() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let repo = r.git.repo();
        let author =
            git2::Signature::now("Author", "author@example.com").unwrap();
        let local = repo
            .commit(
                None,
                &author,
                &author,
                "local",
                &repo.find_tree(r.tree("b")).unwrap(),
                &[&repo.find_commit(m1).unwrap()],
            )
            .unwrap();

        let input = PullRequestCommits {
            head: m1,
            base: m1,
            target: m1,
            head_tree: r.tree("b"),
            base_tree: r.tree("m1"),
            may_update_base: false,
        };
        let plan = input.plan(&r.git).unwrap();
        let commits = input
            .create(
                &r.git,
                &plan,
                CommitInfo {
                    message: "base",
                    author_from: Some(m1),
                },
                CommitInfo {
                    message: "head",
                    author_from: Some(local),
                },
            )
            .unwrap();

        let head = repo.find_commit(commits.head).unwrap();
        assert_eq!(head.author().name(), Ok("Author"));
        assert_eq!(head.committer().name(), Ok("Test"));
    }

    #[test]
    fn test_cherry_pick_trees() {
        let r = TestRepo::new();
        let m1 = r.commit_tree(r.tree_with(&[("x", "1"), ("y", "1")]), &[]);
        let m2 = r.commit_tree(r.tree_with(&[("x", "2"), ("y", "1")]), &[m1]);
        let a = r.commit_tree(r.tree_with(&[("x", "1"), ("y", "a")]), &[m1]);
        let b = r.commit_tree(
            r.tree_with(&[("x", "1"), ("y", "a"), ("z", "b")]),
            &[a],
        );

        // Cherry-pick b (which adds z) onto m2, leaving out a's change.
        let (head_tree, base_tree) =
            cherry_pick(&r.git, m2, r.tree_of(b), r.tree_of(a)).unwrap();

        assert_eq!(base_tree, r.tree_of(m2));
        assert_eq!(
            head_tree,
            r.tree_with(&[("x", "2"), ("y", "1"), ("z", "b")])
        );
    }

    #[test]
    fn test_cherry_pick_conflict() {
        let r = TestRepo::new();
        let m1 = r.commit_tree(r.tree_with(&[("x", "1")]), &[]);
        let m2 = r.commit_tree(r.tree_with(&[("x", "2")]), &[m1]);
        let b = r.commit_tree(r.tree_with(&[("x", "b")]), &[m1]);

        let error =
            cherry_pick(&r.git, m2, r.tree_of(b), r.tree_of(m1)).unwrap_err();
        assert!(error.downcast_ref::<Conflict>().is_some());
    }
}
