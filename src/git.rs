use color_eyre::eyre::{Error, Result, WrapErr as _, bail, eyre};
use std::collections::{HashSet, VecDeque};

use crate::{
    config::{Config, parse_pull_request_field, pull_request_url},
    message::CommitMessage,
    target::Target,
};
use git2::Oid;

#[derive(Debug)]
pub struct PreparedCommit {
    pub oid: Oid,
    pub short_id: String,
    pub parent_oid: Oid,
    pub message: CommitMessage,
    pub pull_request_number: Option<u64>,
    /// Where the commit's Pull Request gets merged (see
    /// `Config::commit_target`)
    pub target: Target,
    /// Whether the commit has a `Spr-Target` trailer
    pub has_target_trailer: bool,
}

impl PreparedCommit {
    /// Whether the commit's target is settled: it names its target, or it
    /// has a Pull Request (whose target is the default target). Commits
    /// without either take the target of the stack they're in.
    pub fn has_fixed_target(&self) -> bool {
        self.has_target_trailer || self.pull_request_number.is_some()
    }
}

/// Check that the commits of a stack have the stack's target. With `all`,
/// all of them must have it; otherwise only those whose target is settled
/// (see `PreparedCommit::has_fixed_target`).
pub fn check_stack_targets<'a>(
    commits: impl IntoIterator<Item = &'a PreparedCommit>,
    target: &Target,
    all: bool,
) -> Result<()> {
    let mismatches: Vec<String> = commits
        .into_iter()
        .filter(|pc| (all || pc.has_fixed_target()) && pc.target != *target)
        .map(|pc| {
            format!(
                "  {} {}: {}{}",
                pc.short_id,
                pc.message.title(),
                pc.target,
                if pc.has_target_trailer {
                    ""
                } else {
                    " (no Spr-Target trailer)"
                }
            )
        })
        .collect();
    if mismatches.is_empty() {
        return Ok(());
    }
    bail!(
        "These commits have another target than the HEAD commit \
         ({target}):\n{}\n\
         All commits of a stack must have the same target. Fix their \
         Spr-Target trailers (e.g. with `git rebase --interactive` and \
         `reword`), or give the target with `spr --target`.",
        mismatches.join("\n")
    )
}

#[derive(Clone)]
pub struct Git {
    repo: std::sync::Arc<git2::Repository>,
    hooks: std::sync::Arc<git2_ext::hooks::Hooks>,
}

impl Git {
    pub fn new(repo: git2::Repository) -> Result<Self> {
        let hooks = git2_ext::hooks::Hooks::with_repo(&repo)
            .wrap_err("Reading the Git hooks configuration failed")?;
        Ok(Self {
            hooks: std::sync::Arc::new(hooks),
            #[allow(clippy::arc_with_non_send_sync)]
            repo: std::sync::Arc::new(repo),
        })
    }

    pub fn repo(&self) -> &std::sync::Arc<git2::Repository> {
        &self.repo
    }

    fn hooks(&self) -> &git2_ext::hooks::Hooks {
        self.hooks.as_ref()
    }

    pub fn get_commit_oids(&self, master_oid: Oid) -> Result<Vec<Oid>> {
        self.get_commit_oids_between(master_oid, self.head()?)
    }

    /// The commits reachable from `head` but not from `target`, from bottom
    /// to top.
    pub fn get_commit_oids_between(
        &self,
        target: Oid,
        head: Oid,
    ) -> Result<Vec<Oid>> {
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL.union(git2::Sort::REVERSE))?;
        walk.push(head)?;
        walk.hide(target)?;

        Ok(walk.collect::<std::result::Result<Vec<Oid>, _>>()?)
    }

    pub fn get_prepared_commits(
        &self,
        config: &Config,
        master_oid: Oid,
    ) -> Result<Vec<PreparedCommit>> {
        self.get_commit_oids(master_oid)?
            .into_iter()
            .map(|oid| self.prepare_commit(config, oid))
            .collect()
    }

    pub fn rewrite_commit_messages(
        &self,
        commits: &mut [PreparedCommit],
        mut limit: Option<usize>,
    ) -> Result<()> {
        if commits.is_empty() {
            return Ok(());
        }

        let mut parent_oid: Option<Oid> = None;
        let mut updating = false;
        let mut message: String;
        let first_parent = commits[0].parent_oid;
        // The commits end at the current HEAD. We only move the branch if it
        // still points there when we're done: the user might have made
        // commits while spr was running (e.g. waiting at a prompt).
        let expected_head = commits[commits.len() - 1].oid;
        let mut rewrites = Vec::new();

        for prepared_commit in commits.iter_mut() {
            let commit = self.repo.find_commit(prepared_commit.oid)?;
            if limit != Some(0) {
                message = prepared_commit.message.to_string();
                if Some(&message[..]) != commit.message().ok() {
                    updating = true;
                }
            } else {
                if !updating {
                    return Ok(());
                }
                message = String::from_utf8_lossy(commit.message_bytes())
                    .into_owned();
            }
            limit = limit.map(|n| if n > 0 { n - 1 } else { 0 });

            if updating {
                let new_oid = self.repo.commit(
                    None,
                    &commit.author(),
                    &commit.committer(),
                    &message[..],
                    &commit.tree()?,
                    &[&self
                        .repo
                        .find_commit(parent_oid.unwrap_or(first_parent))?],
                )?;
                rewrites.push((prepared_commit.oid, new_oid));
                prepared_commit.oid = new_oid;
                parent_oid = Some(new_oid);
            } else {
                parent_oid = Some(prepared_commit.oid);
            }
        }

        if updating && let Some(oid) = parent_oid {
            let reference = self.repo.find_reference("HEAD")?.resolve()?;
            let name = reference.name()?.to_string();
            self.repo
                .reference_matching(
                    &name,
                    oid,
                    true,
                    expected_head,
                    "spr updated commit messages",
                )
                .map_err(|_| {
                    eyre!(
                        "The current branch was changed while spr was \
                         running, so spr did not update the local commit \
                         messages (e.g. with links to new Pull Requests). The \
                         updated commits end at {oid}. To put your new \
                         commits on top of them, run:\n  \
                         git rebase --onto {oid} {expected_head}"
                    )
                })?;
            self.hooks()
                .run_post_rewrite_rebase(self.repo.as_ref(), &rewrites);
        }

        Ok(())
    }

    pub fn rebase_commits(
        &self,
        commits: &mut [PreparedCommit],
        mut new_parent_oid: git2::Oid,
    ) -> Result<()> {
        if commits.is_empty() {
            return Ok(());
        }
        let hooks = self.hooks();

        for prepared_commit in commits.iter_mut() {
            let new_parent_commit = self.repo.find_commit(new_parent_oid)?;
            let commit = self.repo.find_commit(prepared_commit.oid)?;

            let mut index = self.repo.cherrypick_commit(
                &commit,
                &new_parent_commit,
                0,
                None,
            )?;
            if index.has_conflicts() {
                bail!("Rebase failed due to merge conflicts");
            }

            let tree_oid = index.write_tree_to(self.repo.as_ref())?;
            if tree_oid == new_parent_commit.tree_id() {
                // Rebasing makes this an empty commit. This is probably because
                // we just landed this commit. So we should run a hook as this
                // commit (the local pre-land commit) having been rewritten into
                // the parent (the freshly landed and pulled commit). Although
                // this behaviour is tuned around a land operation, it's in
                // general not an unreasoanble thing for a rebase, ala git
                // rebase --interactive and fixups etc.
                hooks.run_post_rewrite_rebase(
                    self.repo.as_ref(),
                    &[(prepared_commit.oid, new_parent_oid)],
                );
                continue;
            }
            let tree = self.repo.find_tree(tree_oid)?;

            new_parent_oid = self.repo.commit(
                None,
                &commit.author(),
                &commit.committer(),
                String::from_utf8_lossy(commit.message_bytes()).as_ref(),
                &tree,
                &[&new_parent_commit],
            )?;
            hooks.run_post_rewrite_rebase(
                self.repo.as_ref(),
                &[(prepared_commit.oid, new_parent_oid)],
            );
        }

        self.move_head(new_parent_oid, "spr rebased")
    }

    /// Drop the given commits, which have been landed, from the current
    /// branch, by moving it to `landed_oid`, the commit on master that
    /// contains them. The commits must be the top commits of the branch.
    pub fn drop_landed_commits(
        &self,
        commits: &[PreparedCommit],
        landed_oid: Oid,
    ) -> Result<()> {
        // Let hooks know that the local commits were rewritten into the
        // landed commit, as a rebase would (see `rebase_commits`).
        let rewrites: Vec<_> = commits
            .iter()
            .map(|prepared_commit| (prepared_commit.oid, landed_oid))
            .collect();
        self.hooks()
            .run_post_rewrite_rebase(self.repo.as_ref(), &rewrites);

        self.move_head(landed_oid, "spr landed")
    }

    /// Replace the commits of the current branch (or HEAD, if detached) up
    /// to `old_head` with the rewritten ones up to `new_head` (e.g. amended
    /// commits), and check out `new_head`. Fails without changing anything
    /// if the branch doesn't point at `old_head` anymore. `rewrites` maps old
    /// to new commits, for the `post-rewrite` hook.
    pub fn replace_head(
        &self,
        old_head: Oid,
        new_head: Oid,
        rewrites: &[(Oid, Oid)],
        reflog_message: &str,
    ) -> Result<()> {
        let reference = self.repo.head()?.resolve()?;
        let name = reference.name()?.to_string();
        if reference.target() != Some(old_head) {
            bail!("The current branch was changed while spr was running");
        }

        let new_commit = self.repo.find_commit(new_head)?;
        self.repo.checkout_tree(new_commit.as_object(), None)?;
        self.repo
            .reference_matching(&name, new_head, true, old_head, reflog_message)
            .map_err(|_| {
                eyre!("The current branch was changed while spr was running")
            })?;
        self.hooks()
            .run_post_rewrite_rebase(self.repo.as_ref(), rewrites);

        Ok(())
    }

    /// Check out the given commit and point the current branch (or HEAD, if
    /// detached) at it.
    fn move_head(&self, new_oid: Oid, reflog_message: &str) -> Result<()> {
        let new_commit = self.repo.find_commit(new_oid)?;

        // Get and resolve the HEAD reference. This will be either a reference
        // to a branch ('refs/heads/...') or 'HEAD' if the head is detached.
        let mut reference = self.repo.head()?.resolve()?;

        // Checkout the tree of the top commit of the rebased branch. This can
        // fail if there are local changes in the worktree that collide with
        // files that need updating in order to check out the rebased commit. In
        // this case we fail early here, before we update any references. The
        // result is that the worktree is unchanged and neither the branch nor
        // HEAD gets updated. We can just prompt the user to rebase manually.
        // That's a fine solution. If the user tries "git rebase origin/master"
        // straight away, they will find that it also fails because of local
        // worktree changes. Once the user has dealt with those (revert, stash
        // or commit), the rebase should work nicely.
        self.repo
            .checkout_tree(new_commit.as_object(), None)
            .map_err(Error::from)
            .wrap_err(
                "Could not check out rebased branch - please rebase manually",
            )?;

        // Update the reference. The reference may be a branch or "HEAD", if
        // detached. Either way, whatever we are on gets update to point to the
        // new commit.
        reference.set_target(new_oid, reflog_message)?;

        Ok(())
    }

    pub fn head(&self) -> Result<Oid> {
        let oid = self
            .repo
            .head()?
            .resolve()?
            .target()
            .ok_or_else(|| eyre!("Cannot resolve HEAD"))?;

        Ok(oid)
    }

    /// Whether `ancestor` is the same commit as `commit`, or one of its
    /// ancestors
    pub fn is_ancestor(&self, ancestor: Oid, commit: Oid) -> Result<bool> {
        Ok(ancestor == commit
            || self.repo.graph_descendant_of(commit, ancestor)?)
    }

    pub fn resolve_reference(&self, reference: &str) -> Result<Oid> {
        let result =
            self.repo.find_reference(reference)?.peel_to_commit()?.id();

        Ok(result)
    }

    /// The ref recording the expected head of the Pull Request of the local
    /// commit with the given ID (`Spr-Id` trailer): the Pull Request head spr
    /// last pushed, or otherwise found the local commit to be consistent
    /// with. The ID must be valid (`CommitMessage::spr_id` only returns valid
    /// ones), as it becomes part of the ref name.
    fn expected_head_ref(spr_id: &str) -> Result<String> {
        if !crate::message::is_valid_spr_id(spr_id) {
            bail!("Invalid Spr-Id: {spr_id}");
        }
        Ok(format!("refs/spr/{spr_id}/head"))
    }

    /// The recorded expected head of the Pull Request of the local commit
    /// with the given ID, if there is one (see `expected_head_ref`)
    pub fn get_expected_head(&self, spr_id: &str) -> Result<Option<Oid>> {
        match self.repo.find_reference(&Self::expected_head_ref(spr_id)?) {
            Ok(reference) => Ok(reference.target()),
            Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Record the expected head of the Pull Request of the local commit with
    /// the given ID (see `expected_head_ref`)
    pub fn set_expected_head(&self, spr_id: &str, head: Oid) -> Result<()> {
        self.repo.reference(
            &Self::expected_head_ref(spr_id)?,
            head,
            true,
            "spr: expected head of Pull Request",
        )?;
        Ok(())
    }

    /// Delete the record of the expected head of the Pull Request of the
    /// local commit with the given ID, if there is one (see
    /// `expected_head_ref`)
    pub fn delete_expected_head(&self, spr_id: &str) -> Result<()> {
        match self.repo.find_reference(&Self::expected_head_ref(spr_id)?) {
            Ok(mut reference) => Ok(reference.delete()?),
            Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn prepare_commit(
        &self,
        config: &Config,
        oid: Oid,
    ) -> Result<PreparedCommit> {
        let commit = self.repo.find_commit(oid)?;

        if commit.parent_count() != 1 {
            bail!("Parent commit count != 1");
        }

        let parent_oid = commit.parent_id(0)?;

        let message_text =
            String::from_utf8_lossy(commit.message_bytes()).into_owned();

        let short_id =
            commit.as_object().short_id()?.as_str().unwrap().to_string();
        drop(commit);

        let mut message = CommitMessage::parse(&message_text);

        let (target, has_target_trailer) =
            config.commit_target(&message).wrap_err_with(|| {
                format!("Commit {short_id} ({})", message.title())
            })?;
        let repository = target.repository();

        // A Pull-request trailer that doesn't refer to a Pull Request in the
        // commit's target repository (e.g. on a commit cherry-picked from
        // another one) is an error: dropping it silently would lose it, and
        // keeping it would put a wrong link into the Pull Request spr creates
        // for the commit.
        let pull_request_number = match message.get_trailer("Pull-request") {
            Some(text) if !text.trim().is_empty() => {
                Some(parse_pull_request_field(repository, text).ok_or_else(
                    || {
                        eyre!(
                            "Commit {short_id} ({title}) has a Pull-request \
                             trailer that doesn't refer to a Pull Request in \
                             {repository}: {text}\n\
                             Remove it from the commit message (e.g. with \
                             `git rebase --interactive` and `reword`), and \
                             spr creates a new Pull Request for the commit.",
                            title = message.title(),
                        )
                    },
                )?)
            }
            _ => None,
        };

        if let Some(number) = pull_request_number {
            message.set_trailer(
                "Pull-request".to_string(),
                pull_request_url(repository, number),
            );
        } else {
            message.remove_trailer("Pull-request");
        }

        Ok(PreparedCommit {
            oid,
            short_id,
            parent_oid,
            message,
            pull_request_number,
            target,
            has_target_trailer,
        })
    }

    pub fn get_all_ref_names(&self) -> Result<HashSet<String>> {
        let result: std::result::Result<HashSet<_>, _> = self
            .repo
            .references()?
            .names()
            .map(|r| r.map(String::from))
            .collect();

        Ok(result?)
    }

    pub fn get_pr_patch_branch_name(&self, pr_number: u64) -> Result<String> {
        let ref_names = self.get_all_ref_names()?;
        let default_name = format!("PR-{}", pr_number);
        if !ref_names.contains(&format!("refs/heads/{}", default_name)) {
            return Ok(default_name);
        }

        let mut count = 1;
        loop {
            let name = format!("PR-{}-{}", pr_number, count);
            if !ref_names.contains(&format!("refs/heads/{}", name)) {
                return Ok(name);
            }
            count += 1;
        }
    }

    pub fn get_tree_oid_for_commit(&self, oid: Oid) -> Result<Oid> {
        let tree_oid = self.repo.find_commit(oid)?.tree_id();

        Ok(tree_oid)
    }

    pub fn find_master_base(
        &self,
        commit_oid: Oid,
        master_oid: Oid,
    ) -> Result<Option<Oid>> {
        let mut commit_ancestors = HashSet::new();
        let mut commit_oid = Some(commit_oid);
        let mut master_ancestors = HashSet::new();
        let mut master_queue = VecDeque::new();
        master_ancestors.insert(master_oid);
        master_queue.push_back(master_oid);

        while !(commit_oid.is_none() && master_queue.is_empty()) {
            if let Some(oid) = commit_oid {
                if master_ancestors.contains(&oid) {
                    return Ok(Some(oid));
                }
                commit_ancestors.insert(oid);
                let commit = self.repo.find_commit(oid)?;
                commit_oid = match commit.parent_count() {
                    0 => None,
                    l => Some(commit.parent_id(l - 1)?),
                };
            }

            if let Some(oid) = master_queue.pop_front() {
                if commit_ancestors.contains(&oid) {
                    return Ok(Some(oid));
                }
                let commit = self.repo.find_commit(oid)?;
                for oid in commit.parent_ids() {
                    if !master_ancestors.contains(&oid) {
                        master_queue.push_back(oid);
                        master_ancestors.insert(oid);
                    }
                }
            }
        }

        Ok(None)
    }

    /// Create a commit with the given message, tree and parents. The author
    /// is the author of `original_commit_oid` (with the current time), or,
    /// if not given, the current user, like the committer.
    pub fn create_derived_commit(
        &self,
        original_commit_oid: Option<Oid>,
        message: &str,
        tree_oid: Oid,
        parent_oids: &[Oid],
    ) -> Result<Oid> {
        let original_commit = original_commit_oid
            .map(|oid| self.repo.find_commit(oid))
            .transpose()?;
        let tree = self.repo.find_tree(tree_oid)?;
        let parents = parent_oids
            .iter()
            .map(|oid| self.repo.find_commit(*oid))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let parent_refs = parents.iter().collect::<Vec<_>>();
        let message = git2::message_prettify(message, None)?;

        // The committer signature should be the default signature (i.e. the
        // current user - as configured in Git as `user.name` and `user.email` -
        // and the timestamp set to now). If the default signature can't be
        // obtained (no user configured), then take the user/email from the
        // existing commit but make a new signature which has a timestamp of
        // now.
        let committer = match (self.repo.signature(), &original_commit) {
            (Ok(signature), _) => signature,
            (Err(_), Some(original_commit)) => git2::Signature::now(
                String::from_utf8_lossy(
                    original_commit.committer().name_bytes(),
                )
                .as_ref(),
                String::from_utf8_lossy(
                    original_commit.committer().email_bytes(),
                )
                .as_ref(),
            )?,
            (Err(error), None) => return Err(error.into()),
        };

        // The author signature should reference the same user as the original
        // commit, but we set the timestamp to now, so this commit shows up in
        // GitHub's timeline in the right place.
        let author = match &original_commit {
            Some(original_commit) => git2::Signature::now(
                String::from_utf8_lossy(original_commit.author().name_bytes())
                    .as_ref(),
                String::from_utf8_lossy(original_commit.author().email_bytes())
                    .as_ref(),
            )?,
            None => committer.clone(),
        };

        let oid = self.repo.commit(
            None,
            &author,
            &committer,
            &message,
            &tree,
            &parent_refs[..],
        )?;

        Ok(oid)
    }

    pub fn check_no_uncommitted_changes(&self) -> Result<()> {
        let mut opts = git2::StatusOptions::new();
        opts.include_ignored(false).include_untracked(false);
        if self.repo.statuses(Some(&mut opts))?.is_empty() {
            Ok(())
        } else {
            Err(eyre!(
                "There are uncommitted changes. Stash or amend them first"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::TestRepo;

    #[test]
    fn test_expected_head_records() {
        let r = TestRepo::new();
        let git = &r.git;
        let m1 = r.commit("m1", &[]);
        let a = r.commit("a", &[m1]);
        let id = crate::message::new_spr_id();

        assert_eq!(git.get_expected_head(&id).unwrap(), None);
        // Deleting a record that doesn't exist is fine
        git.delete_expected_head(&id).unwrap();

        git.set_expected_head(&id, m1).unwrap();
        assert_eq!(git.get_expected_head(&id).unwrap(), Some(m1));
        git.set_expected_head(&id, a).unwrap();
        assert_eq!(git.get_expected_head(&id).unwrap(), Some(a));
        assert_eq!(
            git.repo()
                .find_reference(&format!("refs/spr/{id}/head"))
                .unwrap()
                .target(),
            Some(a)
        );

        git.delete_expected_head(&id).unwrap();
        assert_eq!(git.get_expected_head(&id).unwrap(), None);
    }

    #[test]
    fn test_expected_head_rejects_invalid_ids() {
        let r = TestRepo::new();
        let git = &r.git;
        let m1 = r.commit("m1", &[]);

        for id in ["../../heads/master", "", "abc"] {
            assert!(git.set_expected_head(id, m1).is_err());
            assert!(git.get_expected_head(id).is_err());
            assert!(git.delete_expected_head(id).is_err());
        }
        assert!(git.repo().find_reference("refs/heads/master").is_err());
    }

    /// A repository with branch `main` checked out, pointing at commit a on
    /// top of m1. Returns the repo, m1 and a.
    fn repo_with_branch() -> (TestRepo, Oid, Oid) {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let a = r.commit("a", &[m1]);
        let repo = r.git.repo();
        repo.reference("refs/heads/main", a, true, "test").unwrap();
        repo.set_head("refs/heads/main").unwrap();
        (r, m1, a)
    }

    fn prepared_commit(
        oid: Oid,
        parent_oid: Oid,
        message: &str,
    ) -> PreparedCommit {
        PreparedCommit {
            oid,
            short_id: oid.to_string()[..7].to_string(),
            parent_oid,
            message: CommitMessage::parse(message),
            pull_request_number: None,
            target: "github:o/r:main".parse().unwrap(),
            has_target_trailer: false,
        }
    }

    #[test]
    fn test_check_stack_targets() {
        let (_r, m1, a) = repo_with_branch();
        let main: Target = "github:o/r:main".parse().unwrap();
        let release: Target = "github:o/r:release".parse().unwrap();
        let commit = |target: &Target, trailer: bool, pr: Option<u64>| {
            let mut pc = prepared_commit(a, m1, "A");
            pc.target = target.clone();
            pc.has_target_trailer = trailer;
            pc.pull_request_number = pr;
            pc
        };

        // Commits without trailer or Pull Request take the stack's target,
        // unless all of them must have it
        let free = commit(&main, false, None);
        assert!(check_stack_targets([&free], &release, false).is_ok());
        assert!(check_stack_targets([&free], &release, true).is_err());

        // Commits with a trailer or a Pull Request must have it
        let named = commit(&main, true, None);
        let with_pr = commit(&main, false, Some(1));
        for pc in [&named, &with_pr] {
            assert!(check_stack_targets([pc], &release, false).is_err());
            assert!(check_stack_targets([pc], &main, true).is_ok());
        }

        let error = check_stack_targets([&free, &named], &release, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("A: github:o/r:main (no Spr-Target trailer)"));
        assert!(error.contains("A: github:o/r:main\n"));
    }

    #[test]
    fn test_rewrite_commit_messages() {
        let (r, m1, a) = repo_with_branch();
        let mut commits = [prepared_commit(a, m1, "A\n\nNew message")];

        r.git.rewrite_commit_messages(&mut commits, None).unwrap();

        let head = r.git.head().unwrap();
        assert_ne!(head, a);
        assert_eq!(head, commits[0].oid);
        assert_eq!(r.message_of(head), "A\n\nNew message");
        assert_eq!(r.parents_of(head), vec![m1]);
    }

    #[test]
    fn test_rewrite_commit_messages_branch_changed() {
        let (r, m1, a) = repo_with_branch();
        let mut commits = [prepared_commit(a, m1, "A\n\nNew message")];

        // Meanwhile, the user commits b on top of a
        let b = r.commit("b", &[a]);
        r.git
            .repo()
            .reference("refs/heads/main", b, true, "test")
            .unwrap();

        let error = r.git.rewrite_commit_messages(&mut commits, None);

        assert!(error.is_err());
        assert!(
            format!("{:#}", error.unwrap_err()).contains("git rebase --onto")
        );
        // The branch still points at b
        assert_eq!(r.git.head().unwrap(), b);
    }

    #[test]
    fn test_rewrite_commit_messages_detached_head() {
        let (r, m1, a) = repo_with_branch();
        r.git.repo().set_head_detached(a).unwrap();
        let mut commits = [prepared_commit(a, m1, "A\n\nNew message")];

        r.git.rewrite_commit_messages(&mut commits, None).unwrap();

        let repo = r.git.repo();
        assert!(repo.head_detached().unwrap());
        assert_eq!(r.git.head().unwrap(), commits[0].oid);
        // The branch is left alone
        assert_eq!(
            repo.find_reference("refs/heads/main").unwrap().target(),
            Some(a)
        );
    }

    #[test]
    fn test_prepare_commit_pull_request_trailer() {
        let r = TestRepo::new();
        let m1 = r.commit("m1", &[]);
        let config = crate::config::Config::new(
            "github:acme/codez:master".parse().unwrap(),
            "spr/foo/".into(),
            "xyz".into(),
            false,
            crate::config::MergeMethod::Squash,
            crate::config::StackingMode::BaseBranches,
            true,
            true,
        )
        .unwrap();
        let commit = |message: &str| {
            let repo = r.git.repo();
            let signature = repo.signature().unwrap();
            let tree = repo.find_commit(m1).unwrap().tree().unwrap();
            let parent = repo.find_commit(m1).unwrap();
            let oid = repo
                .commit(
                    None,
                    &signature,
                    &signature,
                    message,
                    &tree,
                    &[&parent],
                )
                .unwrap();
            r.git.prepare_commit(&config, oid)
        };

        // A number, or a link to a Pull Request in this repository (in any
        // case), becomes a link
        for trailer in ["#12", "https://github.com/Acme/Codez/pull/12"] {
            let prepared =
                commit(&format!("A\n\nPull-request: {trailer}\n")).unwrap();
            assert_eq!(prepared.pull_request_number, Some(12));
            assert_eq!(
                prepared.message.get_trailer("Pull-request"),
                Some("https://github.com/acme/codez/pull/12")
            );
        }

        // An empty one is removed
        let prepared =
            commit("A\n\nPull-request:\nSigned-off-by: X\n").unwrap();
        assert_eq!(prepared.pull_request_number, None);
        assert_eq!(prepared.message.get_trailer("Pull-request"), None);

        // One for another repository is an error
        let error = commit(
            "A\n\nPull-request: https://github.com/acme/other/pull/12\n",
        )
        .unwrap_err();
        assert!(error.to_string().contains("acme/other/pull/12"));

        // Without a Spr-Target trailer, the commit has the default target
        let prepared = commit("A\n").unwrap();
        assert_eq!(prepared.target, config.default_target);
        assert!(!prepared.has_target_trailer);
        assert!(!prepared.has_fixed_target());

        // A branch name alone is a branch in the default target's repository
        let prepared =
            commit("A\n\nSpr-Target: release/1.2\nPull-request: #12\n")
                .unwrap();
        assert_eq!(
            prepared.target.to_string(),
            "github:acme/codez:release/1.2"
        );
        assert!(prepared.has_target_trailer);
        assert_eq!(prepared.pull_request_number, Some(12));

        // A Pull-request trailer refers to the target's repository
        let prepared = commit(
            "A\n\nSpr-Target: github:acme/other:main\n\
             Pull-request: https://github.com/acme/other/pull/12\n",
        )
        .unwrap();
        assert_eq!(prepared.pull_request_number, Some(12));
        assert_eq!(
            prepared.message.get_trailer("Pull-request"),
            Some("https://github.com/acme/other/pull/12")
        );

        // An invalid target is an error
        let error = commit("A\n\nSpr-Target: github:acme:main\n").unwrap_err();
        assert!(format!("{error:#}").contains("Invalid Spr-Target"));
    }
}
