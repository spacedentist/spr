//! Helpers for tests

use git2::Oid;

use crate::git::Git;

/// A temporary repository to build commit graphs in
pub struct TestRepo {
    _dir: tempfile::TempDir,
    pub git: Git,
    /// Makes commit messages unique, so commits with the same tree and
    /// parents are still different commits
    counter: std::cell::Cell<usize>,
}

impl Default for TestRepo {
    fn default() -> Self {
        Self::new()
    }
}

impl TestRepo {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "Test").unwrap();
            config.set_str("user.email", "test@example.com").unwrap();
        }
        TestRepo {
            _dir: dir,
            git: Git::new(repo),
            counter: Default::default(),
        }
    }

    /// A tree with a single file `file` with the given content
    pub fn tree(&self, content: &str) -> Oid {
        let repo = self.git.repo();
        let blob = repo.blob(content.as_bytes()).unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        builder.insert("file", blob, 0o100644).unwrap();
        builder.write().unwrap()
    }

    /// A tree with the given files and contents
    pub fn tree_with(&self, files: &[(&str, &str)]) -> Oid {
        let repo = self.git.repo();
        let mut builder = repo.treebuilder(None).unwrap();
        for (name, content) in files {
            let blob = repo.blob(content.as_bytes()).unwrap();
            builder.insert(name, blob, 0o100644).unwrap();
        }
        builder.write().unwrap()
    }

    /// A commit with the given tree and parents
    pub fn commit_tree(&self, tree: Oid, parents: &[Oid]) -> Oid {
        let repo = self.git.repo();
        let tree = repo.find_tree(tree).unwrap();
        let parents: Vec<_> = parents
            .iter()
            .map(|oid| repo.find_commit(*oid).unwrap())
            .collect();
        let parents: Vec<_> = parents.iter().collect();
        let signature = repo.signature().unwrap();
        self.counter.set(self.counter.get() + 1);
        repo.commit(
            None,
            &signature,
            &signature,
            &format!("commit ({})", self.counter.get()),
            &tree,
            &parents,
        )
        .unwrap()
    }

    /// A commit with a tree (see `tree`) and the given parents
    pub fn commit(&self, content: &str, parents: &[Oid]) -> Oid {
        let repo = self.git.repo();
        let tree = repo.find_tree(self.tree(content)).unwrap();
        let parents: Vec<_> = parents
            .iter()
            .map(|oid| repo.find_commit(*oid).unwrap())
            .collect();
        let parents: Vec<_> = parents.iter().collect();
        let signature = repo.signature().unwrap();
        self.counter.set(self.counter.get() + 1);
        repo.commit(
            None,
            &signature,
            &signature,
            &format!("{content} ({})", self.counter.get()),
            &tree,
            &parents,
        )
        .unwrap()
    }

    pub fn tree_of(&self, commit: Oid) -> Oid {
        self.git.get_tree_oid_for_commit(commit).unwrap()
    }

    pub fn parents_of(&self, commit: Oid) -> Vec<Oid> {
        self.git
            .repo()
            .find_commit(commit)
            .unwrap()
            .parent_ids()
            .collect()
    }

    pub fn message_of(&self, commit: Oid) -> String {
        self.git
            .repo()
            .find_commit(commit)
            .unwrap()
            .message()
            .unwrap()
            .to_string()
    }
}
