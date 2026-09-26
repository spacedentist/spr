//! Tests running the `spr plumbing` commands as a binary, in a repository
//! without any spr configuration.

use std::process::{Command, Output};

use git2::{Oid, Repository, Signature};

struct Repo {
    dir: tempfile::TempDir,
    repo: Repository,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        Repo { dir, repo }
    }

    /// A commit with a single file with the given content
    fn commit(&self, content: &str, parents: &[Oid]) -> Oid {
        let blob = self.repo.blob(content.as_bytes()).unwrap();
        let mut builder = self.repo.treebuilder(None).unwrap();
        builder.insert("file", blob, 0o100644).unwrap();
        let tree = self.repo.find_tree(builder.write().unwrap()).unwrap();
        let parents: Vec<_> = parents
            .iter()
            .map(|oid| self.repo.find_commit(*oid).unwrap())
            .collect();
        let parents: Vec<_> = parents.iter().collect();
        let signature = Signature::now("Test", "test@example.com").unwrap();
        self.repo
            .commit(None, &signature, &signature, content, &tree, &parents)
            .unwrap()
    }

    /// Run `spr` in the repository
    fn spr(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_spr"))
            .args(args)
            .current_dir(self.dir.path())
            .output()
            .unwrap()
    }
}

#[test]
fn commit_pr_works_without_spr_configuration() {
    let r = Repo::new();
    let m1 = r.commit("m1", &[]);
    let local = r.commit("b", &[m1]);

    let output = r.spr(&[
        "plumbing",
        "commit-pr",
        "--base",
        &m1.to_string(),
        "--target",
        &m1.to_string(),
        "--local",
        &local.to_string(),
        "-m",
        "initial version",
    ]);

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1], m1.to_string());

    let head = r.repo.find_commit(lines[0].parse().unwrap()).unwrap();
    assert_eq!(head.parent_ids().collect::<Vec<_>>(), vec![m1]);
    assert_eq!(head.tree_id(), r.repo.find_commit(local).unwrap().tree_id());
}

#[test]
fn commit_pr_reports_errors_with_exit_code_and_json() {
    let r = Repo::new();
    let m1 = r.commit("m1", &[]);
    let parent_head = r.commit("a", &[m1]);
    let local_parent = r.commit("amended a", &[m1]);
    let local = r.commit("b", &[local_parent]);

    let output = r.spr(&[
        "plumbing",
        "commit-pr",
        "--base",
        &parent_head.to_string(),
        "--target",
        &m1.to_string(),
        "--local",
        &local.to_string(),
        "--fixed-base",
        "-m",
        "update",
        "--json",
    ]);

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["error"]["kind"], "base-outdated");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("not up to date")
    );
}

#[test]
fn commit_pr_message_required_exit_code() {
    let r = Repo::new();
    let m1 = r.commit("m1", &[]);
    let local = r.commit("b", &[m1]);

    let output = r.spr(&[
        "plumbing",
        "commit-pr",
        "--base",
        &m1.to_string(),
        "--target",
        &m1.to_string(),
        "--local",
        &local.to_string(),
    ]);

    assert_eq!(output.status.code(), Some(3), "{output:?}");
    // Without --json, nothing goes to stdout
    assert!(output.stdout.is_empty());
}
