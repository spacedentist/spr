use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
};

use git2::Oid;

use crate::error::{Error, Result};

#[derive(Clone)]
pub struct GitRemote {
    repo: std::sync::Arc<git2::Repository>,
    url: String,
    auth_token: String,
}

impl GitRemote {
    pub fn new(
        repo: std::sync::Arc<git2::Repository>,
        url: String,
        auth_token: String,
    ) -> Self {
        Self {
            repo,
            url,
            auth_token,
        }
    }

    fn with_connection<F, T>(&self, dir: git2::Direction, func: F) -> Result<T>
    where
        F: FnOnce(&mut git2::RemoteConnection) -> Result<T>,
    {
        let mut remote = self.repo.remote_anonymous(&self.url)?;
        let mut cb = git2::RemoteCallbacks::new();
        cb.credentials(move |_url, _username, _allowed_types| {
            git2::Cred::userpass_plaintext("spr", &self.auth_token)
        });
        let mut connection = remote.connect_auth(dir, Some(cb), None)?;

        func(&mut connection)
    }

    fn get_branches_from_connection(
        connection: &mut git2::RemoteConnection,
    ) -> Result<HashMap<String, Oid>> {
        Ok(connection
            .remote()
            .list()?
            .iter()
            .filter(|&rh| !rh.oid().is_zero())
            .filter_map(|rh| {
                rh.name()
                    .strip_prefix("refs/heads/")
                    .map(|branch| (branch.to_string(), rh.oid()))
            })
            .collect())
    }

    pub fn get_branches(&self) -> Result<HashMap<String, Oid>> {
        self.with_connection(
            git2::Direction::Fetch,
            Self::get_branches_from_connection,
        )
    }

    pub fn fetch_from_remote(
        &self,
        branch_names: &[&str],
        commit_oids: &[Oid],
    ) -> Result<Vec<Option<Oid>>> {
        if branch_names.is_empty() && commit_oids.is_empty() {
            return Ok(Vec::new());
        }

        let mut ref_oids = Vec::<Option<Oid>>::new();
        let mut fetch_oids: HashSet<Oid> =
            commit_oids.iter().cloned().collect();

        self.with_connection(git2::Direction::Fetch, move |connection| {
            if !branch_names.is_empty() {
                let remote_branches =
                    Self::get_branches_from_connection(connection)?;

                for &branch_name in branch_names.iter() {
                    let oid = remote_branches.get(branch_name).cloned();
                    ref_oids.push(oid);
                    fetch_oids.extend(oid.iter());
                }
            }

            if !fetch_oids.is_empty() {
                let fetch_oids =
                    fetch_oids.iter().map(Oid::to_string).collect::<Vec<_>>();

                let mut fetch_options = git2::FetchOptions::new();
                fetch_options.update_fetchhead(false);
                fetch_options.download_tags(git2::AutotagOption::None);
                connection.remote().download(
                    fetch_oids.as_slice(),
                    Some(&mut fetch_options),
                )?;
            }

            Ok(ref_oids)
        })
    }

    pub fn fetch_branch(&self, branch_name: &str) -> Result<Oid> {
        self.fetch_from_remote(&[branch_name], &[])?
            .first()
            .and_then(|&x| x)
            .ok_or_else(|| {
                Error::new(format!("Could not fetch branch '{}'", branch_name,))
            })
    }

    pub fn push_to_remote(&self, refs: &[PushSpec]) -> Result<()> {
        self.with_connection(git2::Direction::Push, move |connection| {
            let push_specs: Vec<String> =
                refs.iter().map(ToString::to_string).collect();
            let push_specs: Vec<&str> =
                push_specs.iter().map(String::as_str).collect();

            connection.remote().push(push_specs.as_slice(), None)?;

            Ok(())
        })
    }

    pub fn find_unused_branch_name(
        &self,
        branch_prefix: &str,
        slug: &str,
    ) -> Result<String> {
        let existing_branch_names = self.with_connection(
            git2::Direction::Fetch,
            Self::get_branches_from_connection,
        )?;

        let mut branch_name = format!("{branch_prefix}{slug}");
        let mut suffix = 0;

        loop {
            if !existing_branch_names.contains_key(&branch_name) {
                return Ok(branch_name);
            }

            suffix += 1;
            branch_name = format!("{branch_prefix}{slug}-{suffix}");
        }
    }
}

pub struct PushSpec<'a> {
    pub oid: Option<Oid>,
    pub remote_ref: &'a str,
}

impl<'a> std::fmt::Display for PushSpec<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(oid) = self.oid {
            oid.fmt(f)?;
        }
        f.write_char(':')?;
        f.write_str(self.remote_ref)
    }
}
