use std::collections::HashSet;

use crate::{
    config::Config,
    error::{Error, Result, ResultExt},
    message::{
        build_commit_message, parse_message, MessageSection, MessageSectionsMap,
    },
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
    repo: std::rc::Rc<git2::Repository>,
}

impl Git {
    pub fn new(repo: git2::Repository) -> Self {
        Self {
            repo: std::rc::Rc::new(repo),
        }
    }

    pub fn get_commit_oids(&self, master_ref: &str) -> Result<Vec<Oid>> {
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL.union(git2::Sort::REVERSE))?;
        walk.push_head()?;
        walk.hide_ref(master_ref)?;

        Ok(walk.collect::<std::result::Result<Vec<Oid>, _>>()?)
    }

    pub fn get_prepared_commits(
        &self,
        config: &Config,
    ) -> Result<Vec<PreparedCommit>> {
        self.get_commit_oids(&config.remote_master_ref)?
            .iter()
            .map(|oid| self.prepare_commit(config, *oid))
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

        for prepared_commit in commits.iter_mut() {
            let commit = self.repo.find_commit(prepared_commit.oid)?;
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
                prepared_commit.oid = new_oid;
                parent_oid = Some(new_oid);
            } else {
                parent_oid = Some(prepared_commit.oid);
            }
        }

        if updating {
            if let Some(oid) = parent_oid {
                self.repo
                    .find_reference("HEAD")?
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
                return Err(Error::new("Rebase failed due to merge conflicts"));
            }

            let tree_oid = index.write_tree_to(&self.repo)?;
            if tree_oid == new_parent_commit.tree_id() {
                // Rebasing makes this an empty commit. We skip it, i.e. don't
                // add it to the rebased branch.
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
        }

        let new_oid = new_parent_oid;
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

    pub fn resolve_reference(&self, reference: &str) -> Result<Oid> {
        let result =
            self.repo.find_reference(reference)?.peel_to_commit()?.id();

        Ok(result)
    }

    pub async fn fetch_commit_from_remote(
        &self,
        commit_oid: git2::Oid,
        remote: String,
    ) -> Result<()> {
        let errored = self.repo.find_commit(commit_oid).is_err();

        if errored {
            let exit_code = async_process::Command::new("git")
                .arg("fetch")
                .arg("--no-write-fetch-head")
                .arg("--")
                .arg(&remote)
                .arg(format!("{}", commit_oid))
                .spawn()?
                .status()
                .await?;

            if !exit_code.success() {
                return Err(Error::new("git fetch failed"));
            }
        }

        Ok(())
    }

    pub fn prepare_commit(
        &self,
        config: &Config,
        oid: Oid,
    ) -> Result<PreparedCommit> {
        let commit = self.repo.find_commit(oid)?;

        if commit.parent_count() != 1 {
            return Err(Error::new("Parent commit count != 1"));
        }

        let parent_oid = commit.parent_id(0)?;

        let message =
            String::from_utf8_lossy(commit.message_bytes()).into_owned();

        let short_id =
            commit.as_object().short_id()?.as_str().unwrap().to_string();
        drop(commit);

        let mut message = parse_message(&message, MessageSection::Title);

        let pull_request_number = message
            .get(&MessageSection::PullRequest)
            .map(|text| config.parse_pull_request_field(text))
            .flatten();

        if let Some(number) = pull_request_number {
            message.insert(
                MessageSection::PullRequest,
                config.pull_request_url(number),
            );
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
            .repo
            .references()?
            .names()
            .map(|r| r.map(String::from))
            .collect();

        Ok(result?)
    }

    pub fn cherrypick(&self, oid: Oid, base_oid: Oid) -> Result<git2::Index> {
        let commit = self.repo.find_commit(oid)?;
        let base_commit = self.repo.find_commit(base_oid)?;

        Ok(self
            .repo
            .cherrypick_commit(&commit, &base_commit, 0, None)?)
    }

    pub fn write_index(&self, mut index: git2::Index) -> Result<Oid> {
        Ok(index.write_tree_to(&*self.repo)?)
    }

    pub fn get_tree_oid_for_commit(&self, oid: Oid) -> Result<Oid> {
        let tree_oid = self.repo.find_commit(oid)?.tree_id();

        Ok(tree_oid)
    }

    pub fn is_based_on(&self, commit_oid: Oid, base_oid: Oid) -> Result<bool> {
        let mut commit = self.repo.find_commit(commit_oid)?;

        loop {
            if commit.parent_count() == 0 {
                return Ok(false);
            } else if commit.parent_count() == 1 {
                let parent_oid = commit.parent_id(0)?;
                if parent_oid == base_oid {
                    return Ok(true);
                }
                commit = self.repo.find_commit(parent_oid)?;
            } else {
                return Ok(commit
                    .parent_ids()
                    .any(|parent_oid| parent_oid == base_oid));
            }
        }
    }

    pub fn create_pull_request_commit(
        &self,
        original_commit_oid: Oid,
        message: Option<&str>,
        tree_oid: Oid,
        parent_oids: &[Oid],
    ) -> Result<Oid> {
        let original_commit = self.repo.find_commit(original_commit_oid)?;
        let tree = self.repo.find_tree(tree_oid)?;
        let parents = parent_oids
            .iter()
            .map(|oid| self.repo.find_commit(*oid))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let parent_refs = parents.iter().collect::<Vec<_>>();
        let message = if let Some(text) = message {
            format!("{}\n", text.trim())
        } else {
            "Initial version\n".into()
        };

        let oid = self.repo.commit(
            None,
            &original_commit.author(),
            &original_commit.committer(),
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
            Err(Error::new(
                "There are uncommitted changes. Stash or amend them first",
            ))
        }
    }
}
