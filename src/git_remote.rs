use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
};

use git2::Oid;

use crate::error::Result;

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

    fn cb(&self) -> git2::RemoteCallbacks<'_> {
        let mut cb = git2::RemoteCallbacks::new();
        cb.credentials(move |_url, _username, _allowed_types| {
            git2::Cred::userpass_plaintext("spr", &self.auth_token)
        });

        cb
    }

    pub fn fetch_from_remote(
        &self,
        refs: &[&str],
        commit_oids: &[Oid],
    ) -> Result<Vec<Option<Oid>>> {
        if refs.is_empty() && commit_oids.is_empty() {
            return Ok(Vec::new());
        }

        let mut ref_oids = Vec::<Option<Oid>>::new();
        let mut fetch_oids: HashSet<Oid> =
            commit_oids.iter().cloned().collect();

        let mut remote = self.repo.remote_anonymous(&self.url)?;
        let mut connection = remote.connect_auth(
            git2::Direction::Fetch,
            Some(self.cb()),
            None,
        )?;

        if !refs.is_empty() {
            let remote_refs: HashMap<String, Oid> = connection
                .remote()
                .list()?
                .iter()
                .map(|rh| (rh.name().to_string(), rh.oid()))
                .collect();

            for &ghref in refs.iter() {
                let oid = remote_refs.get(ghref).cloned();
                ref_oids.push(oid);
                if let Some(oid) = oid {
                    fetch_oids.insert(oid);
                }
            }
        }

        if !fetch_oids.is_empty() {
            let fetch_oids =
                fetch_oids.iter().map(Oid::to_string).collect::<Vec<_>>();

            let mut fetch_options = git2::FetchOptions::new();
            fetch_options.update_fetchhead(false);
            fetch_options.download_tags(git2::AutotagOption::None);
            connection
                .remote()
                .download(fetch_oids.as_slice(), Some(&mut fetch_options))?;
        }

        Ok(ref_oids)
    }

    pub fn push_to_remote(&self, refs: &[PushSpec]) -> Result<()> {
        let mut remote = self.repo.remote_anonymous(&self.url)?;
        let mut connection = remote.connect_auth(
            git2::Direction::Push,
            Some(self.cb()),
            None,
        )?;

        let push_specs: Vec<String> =
            refs.iter().map(ToString::to_string).collect();
        let push_specs: Vec<&str> =
            push_specs.iter().map(String::as_str).collect();

        connection.remote().push(push_specs.as_slice(), None)?;

        Ok(())
    }

    pub fn find_unused_branch_name(
        &self,
        branch_prefix: &str,
        slug: &str,
    ) -> Result<String> {
        let mut remote = self.repo.remote_anonymous(&self.url)?;
        let mut connection = remote.connect_auth(
            git2::Direction::Fetch,
            Some(self.cb()),
            None,
        )?;

        let existing_ref_names: HashSet<String> = connection
            .remote()
            .list()?
            .iter()
            .map(|rh| rh.name().to_string())
            .collect();

        let mut branch_name = format!("{branch_prefix}{slug}");
        let mut suffix = 0;

        loop {
            let remote_ref = format!("refs/heads/{branch_name}");

            if !existing_ref_names.contains(&remote_ref) {
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
