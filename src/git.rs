use std::collections::HashSet;

use crate::{
    config::Config,
    error::{Error, Result},
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
    repo: std::rc::Rc<async_lock::Mutex<git2::Repository>>,
}

impl Git {
    pub fn new(repo: git2::Repository) -> Self {
        Self {
            repo: std::rc::Rc::new(async_lock::Mutex::new(repo)),
        }
    }

    pub async fn get_commit_oids(&self, master_ref: &str) -> Result<Vec<Oid>> {
        let repo = self.repo.lock().await;

        let mut walk = repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL.union(git2::Sort::REVERSE))?;
        walk.push_head()?;
        walk.hide_ref(master_ref)?;

        Ok(walk.collect::<std::result::Result<Vec<Oid>, _>>()?)
    }

    pub async fn get_prepared_commits(
        &self,
        config: &Config,
    ) -> Result<Vec<PreparedCommit>> {
        let commit_oids =
            self.get_commit_oids(&config.remote_master_ref).await?;
        let mut prepared_commits = Vec::<crate::git::PreparedCommit>::new();
        for oid in commit_oids {
            prepared_commits.push(self.prepare_commit(config, oid).await?);
        }

        Ok(prepared_commits)
    }

    pub async fn rewrite_commit_messages(
        &self,
        commits: &mut [PreparedCommit],
        mut limit: Option<usize>,
    ) -> Result<()> {
        if commits.is_empty() {
            return Ok(());
        }

        let repo = self.repo.lock().await;

        let mut parent_oid: Option<Oid> = None;
        let mut updating = false;
        let mut message: String;
        let first_parent = commits[0].parent_oid;

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

    pub async fn resolve_reference(&self, reference: &str) -> Result<Oid> {
        let repo = self.repo.lock().await;

        let result = repo.find_reference(reference)?.peel_to_commit()?.id();

        Ok(result)
    }

    pub async fn fetch_commit_from_remote(
        &self,
        commit_oid: git2::Oid,
        remote: String,
    ) -> Result<()> {
        let errored = self.repo.lock().await.find_commit(commit_oid).is_err();

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

    pub async fn prepare_commit(
        &self,
        config: &Config,
        oid: Oid,
    ) -> Result<PreparedCommit> {
        let repo = self.repo.lock().await;
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

    pub async fn get_all_ref_names(&self) -> Result<HashSet<String>> {
        let repo = self.repo.lock().await;

        let result: std::result::Result<HashSet<_>, _> = repo
            .references()?
            .names()
            .map(|r| r.map(String::from))
            .collect();

        Ok(result?)
    }

    pub async fn cherrypick(
        &self,
        oid: Oid,
        base_oid: Oid,
    ) -> Result<git2::Index> {
        let repo = self.repo.lock().await;

        let commit = repo.find_commit(oid)?;
        let base_commit = repo.find_commit(base_oid)?;

        Ok(repo.cherrypick_commit(&commit, &base_commit, 0, None)?)
    }

    pub async fn write_index(&self, mut index: git2::Index) -> Result<Oid> {
        let repo = self.repo.lock().await;

        Ok(index.write_tree_to(&*repo)?)
    }

    pub async fn get_tree_oid_for_commit(&self, oid: Oid) -> Result<Oid> {
        let repo = self.repo.lock().await;

        let tree_oid = repo.find_commit(oid)?.tree_id();

        Ok(tree_oid)
    }

    pub async fn is_based_on(
        &self,
        commit_oid: Oid,
        base_oid: Oid,
    ) -> Result<bool> {
        let repo = self.repo.lock().await;

        let mut commit = repo.find_commit(commit_oid)?;

        loop {
            if commit.parent_count() == 0 {
                return Ok(false);
            } else if commit.parent_count() == 1 {
                let parent_oid = commit.parent_id(0)?;
                if parent_oid == base_oid {
                    return Ok(true);
                }
                commit = repo.find_commit(parent_oid)?;
            } else {
                return Ok(commit
                    .parent_ids()
                    .any(|parent_oid| parent_oid == base_oid));
            }
        }
    }

    pub async fn create_pull_request_commit(
        &self,
        original_commit_oid: Oid,
        message: Option<&str>,
        tree_oid: Oid,
        parent_oids: &[Oid],
    ) -> Result<Oid> {
        let repo = self.repo.lock().await;

        let original_commit = repo.find_commit(original_commit_oid)?;
        let tree = repo.find_tree(tree_oid)?;
        let parents = parent_oids
            .iter()
            .map(|oid| repo.find_commit(*oid))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let parent_refs = parents.iter().collect::<Vec<_>>();
        let message = if let Some(text) = message {
            format!("{}\n", text.trim())
        } else {
            "Initial version\n".into()
        };

        let oid = repo.commit(
            None,
            &original_commit.author(),
            &original_commit.committer(),
            &message,
            &tree,
            &parent_refs[..],
        )?;

        Ok(oid)
    }
}
