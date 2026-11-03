//! Local Git retrieval behavior; no network or external credentials.
use ods_provider_git::LocalGitCode;
use ods_sdk::contracts::code_reader::{CodeReadError, CodeReader};
use ods_sdk::contracts::producing_code::{CodeLocation, Revision};
use std::{collections::BTreeMap, path::Path, process::Command, time::Duration};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}
fn fixture() -> (tempfile::TempDir, String) {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    std::fs::write(root.path().join("load.sql"), "select 1 as value\n").unwrap();
    git(root.path(), &["add", "load.sql"]);
    git(
        root.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    let commit = git(root.path(), &["rev-parse", "HEAD"]);
    (root, commit)
}
fn reader(root: &Path, bytes: usize) -> LocalGitCode {
    LocalGitCode::new(
        BTreeMap::from([("ingestion".into(), root.to_path_buf())]),
        bytes,
        Duration::from_secs(5),
    )
    .unwrap()
}
fn location(commit: &str, path: &str) -> CodeLocation {
    CodeLocation::new(
        "ingestion",
        path,
        Revision::Exact {
            commit: commit.into(),
            run: "producer-1".into(),
        },
    )
}
#[tokio::test]
async fn reads_the_commit_not_a_dirty_working_file() {
    let (root, commit) = fixture();
    std::fs::write(root.path().join("load.sql"), "PRIVATE_WORKTREE").unwrap();
    let result = reader(root.path(), 1024)
        .read_code(&location(&commit, "load.sql"), Some("producer-1"))
        .await
        .unwrap();
    assert_eq!(result.text(), "select 1 as value\n");
    assert_eq!(result.commit(), commit);
    assert!(!format!("{result:?}").contains("select 1"));
}
#[tokio::test]
async fn rejects_wrong_runs_unapproved_repositories_and_missing_revisions() {
    let (root, commit) = fixture();
    let reader = reader(root.path(), 1024);
    let mut code = location(&commit, "load.sql");
    assert_eq!(
        reader.read_code(&code, Some("other")).await.unwrap_err(),
        CodeReadError::RevisionMismatch
    );
    code.repository = "unapproved".into();
    assert_eq!(
        reader
            .read_code(&code, Some("producer-1"))
            .await
            .unwrap_err(),
        CodeReadError::RepositoryNotAllowed
    );
    code.repository = "ingestion".into();
    code.revision = Revision::Unknown;
    assert_eq!(
        reader.read_code(&code, None).await.unwrap_err(),
        CodeReadError::RevisionRequired
    );
}
#[tokio::test]
async fn missing_oversized_and_traversing_paths_are_explicit_errors() {
    let (root, commit) = fixture();
    let reader = reader(root.path(), 4);
    assert_eq!(
        reader
            .read_code(&location(&commit, "load.sql"), Some("producer-1"))
            .await
            .unwrap_err(),
        CodeReadError::TooLarge
    );
    assert_eq!(
        reader
            .read_code(&location(&commit, "absent.sql"), Some("producer-1"))
            .await
            .unwrap_err(),
        CodeReadError::PathMissing
    );
    assert_eq!(
        reader
            .read_code(&location(&commit, "../secret"), Some("producer-1"))
            .await
            .unwrap_err(),
        CodeReadError::InvalidRequest
    );
}
#[cfg(unix)]
#[tokio::test]
async fn symlink_blobs_are_never_followed_or_returned_as_source() {
    let (root, _) = fixture();
    std::os::unix::fs::symlink("/etc/passwd", root.path().join("secret.sql")).unwrap();
    git(root.path(), &["add", "secret.sql"]);
    git(
        root.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "link",
        ],
    );
    let commit = git(root.path(), &["rev-parse", "HEAD"]);
    assert_eq!(
        reader(root.path(), 1024)
            .read_code(&location(&commit, "secret.sql"), Some("producer-1"))
            .await
            .unwrap_err(),
        CodeReadError::UnsupportedEntry
    );
}

#[tokio::test]
async fn conforms_to_the_shared_contract() {
    let (root, commit) = fixture();
    ods_sdk::conformance::code_reader::check(
        &reader(root.path(), 1024),
        &location(&commit, "load.sql"),
        "producer-1",
        "select 1 as value\n",
    )
    .await;
}
#[tokio::test]
async fn unavailable_commit_binary_content_and_linked_layout_are_explicit() {
    let (root, commit) = fixture();
    let source = reader(root.path(), 1024);
    assert_eq!(
        source
            .read_code(&location(&"0".repeat(40), "load.sql"), Some("producer-1"))
            .await
            .unwrap_err(),
        CodeReadError::RevisionMissing
    );
    std::fs::write(root.path().join("binary.sql"), [0xff, 0]).unwrap();
    git(root.path(), &["add", "binary.sql"]);
    git(
        root.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "binary",
        ],
    );
    let binary_commit = git(root.path(), &["rev-parse", "HEAD"]);
    assert_eq!(
        source
            .read_code(&location(&binary_commit, "binary.sql"), Some("producer-1"))
            .await
            .unwrap_err(),
        CodeReadError::NotText
    );
    let linked = tempfile::tempdir().unwrap();
    std::fs::write(linked.path().join(".git"), "gitdir: /outside").unwrap();
    assert_eq!(
        reader(linked.path(), 1024)
            .read_code(&location(&commit, "load.sql"), Some("producer-1"))
            .await
            .unwrap_err(),
        CodeReadError::UnsupportedEntry
    );
}

#[tokio::test]
async fn literal_paths_and_limits_are_enforced() {
    let (root, _) = fixture();
    std::fs::write(root.path().join("[one].sql"), "select 2").unwrap();
    git(root.path(), &["add", "[one].sql"]);
    git(
        root.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "literal",
        ],
    );
    let commit = git(root.path(), &["rev-parse", "HEAD"]);
    assert_eq!(
        reader(root.path(), 8)
            .read_code(&location(&commit, "[one].sql"), Some("producer-1"))
            .await
            .unwrap()
            .text(),
        "select 2"
    );
    assert!(LocalGitCode::new(BTreeMap::new(), 0, Duration::from_secs(1)).is_err());
    let deadline = LocalGitCode::new(
        BTreeMap::from([("ingestion".into(), root.path().to_path_buf())]),
        1024,
        Duration::from_nanos(1),
    )
    .unwrap();
    assert_eq!(
        deadline
            .read_code(&location(&commit, "load.sql"), Some("producer-1"))
            .await
            .unwrap_err(),
        CodeReadError::TimedOut
    );
}

#[test]
fn remote_reader_accepts_only_explicit_credential_free_https_urls() {
    use ods_provider_git::RemoteGitCode;
    for url in [
        "http://example.com/repo",
        "ssh://git@example.com/repo",
        "https://user:token@example.com/repo",
        "https://example.com/repo?token=secret",
        "file:///tmp/repo",
    ] {
        assert!(
            RemoteGitCode::new(
                BTreeMap::from([("repo".into(), url.into())]),
                1024,
                Duration::from_secs(5)
            )
            .is_err(),
            "{url}"
        );
    }
    assert!(
        RemoteGitCode::new(
            BTreeMap::from([("repo".into(), "https://example.com/repo.git".into())]),
            1024,
            Duration::from_secs(5)
        )
        .is_ok()
    );
}
