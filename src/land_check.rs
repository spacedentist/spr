//! Checking that landing a Pull Request does the right thing.
//!
//! Before landing, spr checks that merging the Pull Request into the target
//! branch gives the same result as applying the local commits to it. That
//! way, spr never lands anything but the changes of the local commits, and
//! never lands changes that reviewers haven't seen.

use color_eyre::eyre::Result;
use git2::Oid;

use crate::git::Git;

/// The outcome of the check
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandCheck {
    /// Merging the Pull Request gives the same tree as applying the local
    /// commits: this tree.
    Ok(Oid),
    /// The local commits can't be applied to the target without conflicts.
    LocalConflict,
    /// Merging the Pull Request into the target has conflicts.
    MergeConflict,
    /// Both work, but give different trees.
    Mismatch { local_tree: Oid, merge_tree: Oid },
}

/// Check whether merging `pr_head` into `target` gives the same tree as
/// applying the changes of the local commits `since..local` to `target`.
pub fn land_check(
    git: &Git,
    target: Oid,
    pr_head: Oid,
    local: Oid,
    since: Oid,
) -> Result<LandCheck> {
    let repo = git.repo();
    let target = repo.find_commit(target)?;

    let mut local_index = repo.merge_trees(
        &repo.find_commit(since)?.tree()?,
        &target.tree()?,
        &repo.find_commit(local)?.tree()?,
        None,
    )?;
    if local_index.has_conflicts() {
        return Ok(LandCheck::LocalConflict);
    }
    let local_tree = local_index.write_tree_to(repo)?;

    let mut merge_index =
        repo.merge_commits(&target, &repo.find_commit(pr_head)?, None)?;
    if merge_index.has_conflicts() {
        return Ok(LandCheck::MergeConflict);
    }
    let merge_tree = merge_index.write_tree_to(repo)?;

    Ok(if local_tree == merge_tree {
        LandCheck::Ok(local_tree)
    } else {
        LandCheck::Mismatch {
            local_tree,
            merge_tree,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::TestRepo;

    // Master has m1 and m2 (changing x); the local commit b (adding y) is
    // based on m1, and the Pull Request branch has a commit with b's tree.

    #[test]
    fn test_land_check_ok() {
        let r = TestRepo::new();
        let m1 = r.commit_tree(r.tree_with(&[("x", "1")]), &[]);
        let m2 = r.commit_tree(r.tree_with(&[("x", "2")]), &[m1]);
        let b = r.commit_tree(r.tree_with(&[("x", "1"), ("y", "b")]), &[m1]);
        let pr_head = r.commit_tree(r.tree_of(b), &[m1]);

        assert_eq!(
            land_check(&r.git, m2, pr_head, b, m1).unwrap(),
            LandCheck::Ok(r.tree_with(&[("x", "2"), ("y", "b")]))
        );
    }

    #[test]
    fn test_land_check_stack() {
        let r = TestRepo::new();
        let m1 = r.commit_tree(r.tree_with(&[("x", "1")]), &[]);
        let m2 = r.commit_tree(r.tree_with(&[("x", "2")]), &[m1]);
        let a = r.commit_tree(r.tree_with(&[("x", "1"), ("a", "a")]), &[m1]);
        let b = r.commit_tree(
            r.tree_with(&[("x", "1"), ("a", "a"), ("y", "b")]),
            &[a],
        );
        // Chained Pull Requests: b's Pull Request contains a's
        let pr_a = r.commit_tree(r.tree_of(a), &[m1]);
        let pr_b = r.commit_tree(r.tree_of(b), &[pr_a]);

        assert_eq!(
            land_check(&r.git, m2, pr_b, b, m1).unwrap(),
            LandCheck::Ok(r.tree_with(&[("x", "2"), ("a", "a"), ("y", "b")]))
        );
    }

    #[test]
    fn test_land_check_mismatch() {
        let r = TestRepo::new();
        let m1 = r.commit_tree(r.tree_with(&[("x", "1")]), &[]);
        let b = r.commit_tree(r.tree_with(&[("x", "1"), ("y", "b")]), &[m1]);
        // The Pull Request wasn't updated after amending b
        let pr_head =
            r.commit_tree(r.tree_with(&[("x", "1"), ("y", "old")]), &[m1]);

        assert!(matches!(
            land_check(&r.git, m1, pr_head, b, m1).unwrap(),
            LandCheck::Mismatch { .. }
        ));
    }

    #[test]
    fn test_land_check_conflicts() {
        let r = TestRepo::new();
        let m1 = r.commit_tree(r.tree_with(&[("x", "1")]), &[]);
        let m2 = r.commit_tree(r.tree_with(&[("x", "2")]), &[m1]);
        let b = r.commit_tree(r.tree_with(&[("x", "b")]), &[m1]);
        let pr_head = r.commit_tree(r.tree_of(b), &[m1]);

        assert_eq!(
            land_check(&r.git, m2, pr_head, b, m1).unwrap(),
            LandCheck::LocalConflict
        );

        // The local commit applies, but the Pull Request conflicts
        let local = r.commit_tree(r.tree_with(&[("x", "2")]), &[m1]);
        assert_eq!(
            land_check(&r.git, m2, pr_head, local, m1).unwrap(),
            LandCheck::MergeConflict
        );
    }
}
