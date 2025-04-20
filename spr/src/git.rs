/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::collections::{HashSet, VecDeque};

use crate::{
    config::Config,
    error::{Error, Result, ResultExt},
    github::GitHubBranch,
    message::{
        build_commit_message, parse_message, MessageSection, MessageSectionsMap,
    },
    utils::run_command,
};
use git2::Oid;

#[derive(Debug)]
pub struct PreparedCommit {
    pub oid: Oid,
    pub short_id: String,
    pub parent_oid: Oid,
    pub message: MessageSectionsMap,
    pub pull_request_number: Option<u64>,
}

#[derive(Clone)]
pub struct Git {
    repo: std::sync::Arc<std::sync::Mutex<git2::Repository>>,
    hooks: std::sync::Arc<std::sync::Mutex<git2_ext::hooks::Hooks>>,
}

impl Git {
    pub fn new(repo: git2::Repository) -> Self {
        Self {
            hooks: std::sync::Arc::new(std::sync::Mutex::new(
                git2_ext::hooks::Hooks::with_repo(&repo).unwrap(),
            )),
            repo: std::sync::Arc::new(std::sync::Mutex::new(repo)),
        }
    }

    pub fn repo(&self) -> std::sync::MutexGuard<git2::Repository> {
        self.repo.lock().expect("poisoned mutex")
    }

    fn hooks(&self) -> std::sync::MutexGuard<git2_ext::hooks::Hooks> {
        self.hooks.lock().expect("poisoned mutex")
    }

    pub fn get_commit_oids(&self, master_ref: &str) -> Result<Vec<Oid>> {
        let repo = self.repo();
        let mut walk = repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL.union(git2::Sort::REVERSE))?;
        walk.push_head()?;
        walk.hide_ref(master_ref)?;

        Ok(walk.collect::<std::result::Result<Vec<Oid>, _>>()?)
    }

    pub fn get_prepared_commits(
        &self,
        config: &Config,
    ) -> Result<Vec<PreparedCommit>> {
        self.get_commit_oids(config.master_ref.local())?
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
        let repo = self.repo();
        let hooks = self.hooks();

        for prepared_commit in commits.iter_mut() {
            let commit = repo.find_commit(prepared_commit.oid)?;
            if limit != Some(0) {
                message = build_commit_message(&prepared_commit.message);
                if Some(&message[..]) != commit.message() {
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
                let new_oid = repo.commit(
                    None,
                    &commit.author(),
                    &commit.committer(),
                    &message[..],
                    &commit.tree()?,
                    &[&repo.find_commit(parent_oid.unwrap_or(first_parent))?],
                )?;
                hooks.run_post_rewrite_rebase(
                    &repo,
                    &[(prepared_commit.oid, new_oid)],
                );
                prepared_commit.oid = new_oid;
                parent_oid = Some(new_oid);
            } else {
                parent_oid = Some(prepared_commit.oid);
            }
        }

        if updating {
            if let Some(oid) = parent_oid {
                repo.find_reference("HEAD")?
                    .resolve()?
                    .set_target(oid, "spr updated commit messages")?;
            }
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
        let repo = self.repo();
        let hooks = self.hooks();

        for prepared_commit in commits.iter_mut() {
            let new_parent_commit = repo.find_commit(new_parent_oid)?;
            let commit = repo.find_commit(prepared_commit.oid)?;

            let mut index =
                repo.cherrypick_commit(&commit, &new_parent_commit, 0, None)?;
            if index.has_conflicts() {
                return Err(Error::new("Rebase failed due to merge conflicts"));
            }

            let tree_oid = index.write_tree_to(&repo)?;
            if tree_oid == new_parent_commit.tree_id() {
                // Rebasing makes this an empty commit. This is probably because
                // we just landed this commit. So we should run a hook as this
                // commit (the local pre-land commit) having been rewritten into
                // the parent (the freshly landed and pulled commit). Although
                // this behaviour is tuned around a land operation, it's in
                // general not an unreasoanble thing for a rebase, ala git
                // rebase --interactive and fixups etc.
                hooks.run_post_rewrite_rebase(
                    &repo,
                    &[(prepared_commit.oid, new_parent_oid)],
                );
                continue;
            }
            let tree = repo.find_tree(tree_oid)?;

            new_parent_oid = repo.commit(
                None,
                &commit.author(),
                &commit.committer(),
                String::from_utf8_lossy(commit.message_bytes()).as_ref(),
                &tree,
                &[&new_parent_commit],
            )?;
            hooks.run_post_rewrite_rebase(
                &repo,
                &[(prepared_commit.oid, new_parent_oid)],
            );
        }

        let new_oid = new_parent_oid;
        let new_commit = repo.find_commit(new_oid)?;

        // Get and resolve the HEAD reference. This will be either a reference
        // to a branch ('refs/heads/...') or 'HEAD' if the head is detached.
        let mut reference = repo.head()?.resolve()?;

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
        repo.checkout_tree(new_commit.as_object(), None)
            .map_err(Error::from)
            .reword(
                "Could not check out rebased branch - please rebase manually"
                    .into(),
            )?;

        // Update the reference. The reference may be a branch or "HEAD", if
        // detached. Either way, whatever we are on gets update to point to the
        // new commit.
        reference.set_target(new_oid, "spr rebased")?;

        Ok(())
    }

    pub fn head(&self) -> Result<Oid> {
        let oid = self
            .repo()
            .head()?
            .resolve()?
            .target()
            .ok_or_else(|| Error::new("Cannot resolve HEAD"))?;

        Ok(oid)
    }

    pub fn resolve_reference(&self, reference: &str) -> Result<Oid> {
        let result = self
            .repo()
            .find_reference(reference)?
            .peel_to_commit()?
            .id();

        Ok(result)
    }

    pub async fn fetch_commits_from_remote(
        &self,
        commit_oids: &[git2::Oid],
        remote: &str,
    ) -> Result<()> {
        let missing_commit_oids: Vec<_> = {
            let repo = self.repo();

            commit_oids
                .iter()
                .filter(|oid| repo.find_commit(**oid).is_err())
                .collect()
        };

        if !missing_commit_oids.is_empty() {
            let mut command = tokio::process::Command::new("git");
            command
                .arg("fetch")
                .arg("--no-write-fetch-head")
                .arg("--")
                .arg(remote);

            for oid in missing_commit_oids {
                command.arg(format!("{}", oid));
            }

            run_command(&mut command)
                .await
                .reword("git fetch failed".to_string())?;
        }

        Ok(())
    }

    pub async fn fetch_from_remote(
        refs: &[&GitHubBranch],
        remote: &str,
    ) -> Result<()> {
        if !refs.is_empty() {
            let mut command = tokio::process::Command::new("git");
            command
                .arg("fetch")
                .arg("--no-write-fetch-head")
                .arg("--")
                .arg(remote);

            for ghref in refs {
                command.arg(ghref.on_github());
            }

            run_command(&mut command)
                .await
                .reword("git fetch failed".to_string())?;
        }

        Ok(())
    }

    pub fn prepare_commit(
        &self,
        config: &Config,
        oid: Oid,
    ) -> Result<PreparedCommit> {
        let repo = self.repo();
        let commit = repo.find_commit(oid)?;

        if commit.parent_count() != 1 {
            return Err(Error::new("Parent commit count != 1"));
        }

        let parent_oid = commit.parent_id(0)?;

        let message =
            String::from_utf8_lossy(commit.message_bytes()).into_owned();

        let short_id =
            commit.as_object().short_id()?.as_str().unwrap().to_string();
        drop(commit);
        drop(repo);

        let mut message = parse_message(&message, MessageSection::Title);

        let pull_request_number = message
            .get(&MessageSection::PullRequest)
            .and_then(|text| config.parse_pull_request_field(text));

        if let Some(number) = pull_request_number {
            message.insert(
                MessageSection::PullRequest,
                config.pull_request_url(number),
            );
        } else {
            message.remove(&MessageSection::PullRequest);
        }

        Ok(PreparedCommit {
            oid,
            short_id,
            parent_oid,
            message,
            pull_request_number,
        })
    }

    pub fn get_all_ref_names(&self) -> Result<HashSet<String>> {
        let result: std::result::Result<HashSet<_>, _> = self
            .repo()
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

    pub fn cherrypick(&self, oid: Oid, base_oid: Oid) -> Result<git2::Index> {
        let repo = self.repo();
        let commit = repo.find_commit(oid)?;
        let base_commit = repo.find_commit(base_oid)?;

        Ok(repo.cherrypick_commit(&commit, &base_commit, 0, None)?)
    }

    pub fn write_index(&self, mut index: git2::Index) -> Result<Oid> {
        Ok(index.write_tree_to(&self.repo())?)
    }

    pub fn get_tree_oid_for_commit(&self, oid: Oid) -> Result<Oid> {
        let tree_oid = self.repo().find_commit(oid)?.tree_id();

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
        let repo = self.repo();

        while !(commit_oid.is_none() && master_queue.is_empty()) {
            if let Some(oid) = commit_oid {
                if master_ancestors.contains(&oid) {
                    return Ok(Some(oid));
                }
                commit_ancestors.insert(oid);
                let commit = repo.find_commit(oid)?;
                commit_oid = match commit.parent_count() {
                    0 => None,
                    l => Some(commit.parent_id(l - 1)?),
                };
            }

            if let Some(oid) = master_queue.pop_front() {
                if commit_ancestors.contains(&oid) {
                    return Ok(Some(oid));
                }
                let commit = repo.find_commit(oid)?;
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

    pub fn create_derived_commit(
        &self,
        original_commit_oid: Oid,
        message: &str,
        tree_oid: Oid,
        parent_oids: &[Oid],
    ) -> Result<Oid> {
        let repo = self.repo();
        let original_commit = repo.find_commit(original_commit_oid)?;
        let tree = repo.find_tree(tree_oid)?;
        let parents = parent_oids
            .iter()
            .map(|oid| repo.find_commit(*oid))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let parent_refs = parents.iter().collect::<Vec<_>>();
        let message = git2::message_prettify(message, None)?;

        // The committer signature should be the default signature (i.e. the
        // current user - as configured in Git as `user.name` and `user.email` -
        // and the timestamp set to now). If the default signature can't be
        // obtained (no user configured), then take the user/email from the
        // existing commit but make a new signature which has a timestamp of
        // now.
        let committer = repo.signature().or_else(|_| {
            git2::Signature::now(
                String::from_utf8_lossy(
                    original_commit.committer().name_bytes(),
                )
                .as_ref(),
                String::from_utf8_lossy(
                    original_commit.committer().email_bytes(),
                )
                .as_ref(),
            )
        })?;

        // The author signature should reference the same user as the original
        // commit, but we set the timestamp to now, so this commit shows up in
        // GitHub's timeline in the right place.
        let author = git2::Signature::now(
            String::from_utf8_lossy(original_commit.author().name_bytes())
                .as_ref(),
            String::from_utf8_lossy(original_commit.author().email_bytes())
                .as_ref(),
        )?;

        let oid = repo.commit(
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
        if self.repo().statuses(Some(&mut opts))?.is_empty() {
            Ok(())
        } else {
            Err(Error::new(
                "There are uncommitted changes. Stash or amend them first",
            ))
        }
    }
}
