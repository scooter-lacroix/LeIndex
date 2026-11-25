use super::*;

/// A git child whose environment is ISOLATED from whatever launched the test
/// binary (CodeRabbit round 10, id 4697090082).
///
/// A developer who runs the suite from a git hook hands the test process
/// `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE`, and those override the
/// working directory: fixture commands would then read or mutate the CALLER'S
/// repository. A `TempDir` protects only paths that resolve inside it, which
/// is not enough when the variables point elsewhere.
///
/// The list is NOT repeated here: it comes from
/// [`super::super::git_apply_exec::REPOSITORY_LOCATION_OVERRIDES`], the same
/// constant the production builders scrub. A fixture that cleared a different
/// set could pass while the code under test inherited a variable, and
/// `test_an_inherited_git_dir_cannot_redirect_a_request` poisons the process
/// environment for the whole binary while other tests run — so the fixtures
/// must be immune for exactly the same reasons the handler is.
fn isolated_git(dir: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    // `git_apply_exec` is a SIBLING module of this test module's parent, so it
    // is reached through `super::super`, not `super`.
    super::super::git_apply_exec::scrub_std(&mut command);
    command
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

fn git(dir: &Path, args: &[&str]) {
    let status = isolated_git(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .expect("git available");
    assert!(status.success(), "git {args:?} failed");
}

/// Captured output from the SAME isolated git as [`git`].
///
/// Fixture READS go through here too, never a bare
/// `std::process::Command::new("git")`: the poisoned
/// `GIT_DIR` / `GIT_WORK_TREE` window opened by
/// `test_an_inherited_git_dir_cannot_redirect_a_request` is process-global and
/// overlaps other tests, so an unscrubbed read inspects that test's decoy
/// repository — or fails with "not a git repository" — and the failure lands
/// nowhere near its cause.
fn git_out(dir: &Path, args: &[&str]) -> std::process::Output {
    isolated_git(dir)
        .args(args)
        .output()
        .expect("git available")
}

fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "master"]);
    // Identity lives in the REPOSITORY, not just on the setup commands
    // (CodeRabbit round 10, id 4697090088): the handler under test spawns its
    // own git children, which never see the env above. On a host with
    // `user.useConfigOnly=true` and no global identity those children cannot
    // commit, and a correct handler would fail its own test.
    git(dir.path(), &["config", "user.name", "t"]);
    git(dir.path(), &["config", "user.email", "t@t"]);
    git(dir.path(), &["config", "commit.gpgSign", "false"]);
    std::fs::write(dir.path().join("f.txt"), "a\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "one"]);
    dir
}

/// Codex round 5 (id 4688435762): the token's index digest was taken with
/// `git ls-files` run from whatever `root` the caller named. When that
/// root is a SUBDIRECTORY of the repository, the listing covers only that
/// subtree — yet a `commit` with no `paths` commits the repository-wide
/// staged set. Staging a sibling after the plan therefore left HEAD and
/// the digest untouched, and the reviewed token authorized a commit
/// containing changes the plan never showed.
///
/// The digest must cover the whole worktree index.
#[test]
fn test_index_digest_covers_the_worktree_not_just_the_named_subdirectory() {
    let repo = scratch();
    let toplevel = repo.path();
    let member = toplevel.join("member");
    std::fs::create_dir_all(&member).unwrap();
    git(toplevel, &["config", "commit.gpgSign", "false"]);
    std::fs::write(member.join("a.txt"), "a\n").unwrap();
    std::fs::write(toplevel.join("sibling.txt"), "s\n").unwrap();
    git(toplevel, &["add", "."]);
    git(toplevel, &["commit", "-q", "-m", "two"]);

    let before = index_digest(&member).expect("digest from the subdirectory");
    // The digest of the subdirectory and of the worktree root agree on a
    // clean tree: both name the same repository state.
    assert_eq!(
        before,
        index_digest(toplevel).expect("digest from the root"),
        "a subdirectory's digest must be the worktree's digest"
    );

    // Stage a SIBLING of the subdirectory. A commit issued from `member`
    // with no `paths` would include it, so the digest MUST move.
    std::fs::write(toplevel.join("sibling.txt"), "changed\n").unwrap();
    git(toplevel, &["add", "sibling.txt"]);
    let after = index_digest(&member).expect("digest after staging the sibling");
    assert_ne!(
        before, after,
        "staging a sibling changed what a commit would carry, so the token's state must change too"
    );
    assert_eq!(
        after,
        index_digest(toplevel).expect("root digest after staging"),
        "the subdirectory digest tracks the worktree index"
    );
}

/// Codex round 6 (id 4688809351): the advertised `force-push
/// origin/branch` shape was passed to git as ONE argument, which `git push`
/// reads as a repository name — git refuses it outright ("'origin/main' is
/// not a valid push target"). The plan has to be executable: split it into
/// `<remote> HEAD:refs/heads/<branch>`.
///
/// CodeRabbit round 8 (id 4691119074): the lease must carry the REVIEWED
/// remote oid, so a remote that moved after planning is rejected instead of
/// overwritten.
#[test]
fn test_force_push_argv_is_executable() {
    use super::super::git_handler::force_push_argv;
    // A pinned lease names the branch AND the reviewed oid.
    assert_eq!(
        force_push_argv(
            "origin/main",
            Some("1111111111111111111111111111111111111111")
        ),
        [
            "push",
            "--force-with-lease=refs/heads/main:1111111111111111111111111111111111111111",
            "origin",
            "HEAD:refs/heads/main"
        ]
    );
    assert_eq!(
        force_push_argv(
            "origin/feature/x",
            Some("2222222222222222222222222222222222222222")
        ),
        [
            "push",
            "--force-with-lease=refs/heads/feature/x:2222222222222222222222222222222222222222",
            "origin",
            "HEAD:refs/heads/feature/x"
        ],
        "a slashed branch name must survive whole, in the lease and the refspec"
    );
    // No oid to pin (unborn branch): fall back to the implicit lease
    // rather than dropping the guard.
    assert_eq!(
        force_push_argv("origin/main", None),
        [
            "push",
            "--force-with-lease",
            "origin",
            "HEAD:refs/heads/main"
        ]
    );
    // A malformed oid is not pinned (it would make git reject a lease
    // that can never be satisfied).
    assert_eq!(
        force_push_argv("origin/main", Some("not-an-oid")),
        [
            "push",
            "--force-with-lease",
            "origin",
            "HEAD:refs/heads/main"
        ]
    );
    // A bare remote keeps git's own upstream resolution.
    assert_eq!(
        force_push_argv("origin", Some("1111111111111111111111111111111111111111")),
        ["push", "--force-with-lease", "origin"],
        "a bare remote names no branch to pin"
    );
}

/// …and the split form must actually push, while the single-argument form
/// must not. A local bare remote makes that provable without a network.
#[test]
fn test_force_push_argv_really_pushes_while_the_single_argument_does_not() {
    use super::super::git_handler::force_push_argv;
    let holder = tempfile::tempdir().unwrap();
    let bare = holder.path().join("remote.git");
    let work = holder.path().join("work");
    git(
        holder.path(),
        &["init", "-q", "--bare", bare.to_str().unwrap()],
    );
    git(
        holder.path(),
        &["clone", "-q", bare.to_str().unwrap(), "work"],
    );
    git(&work, &["config", "commit.gpgSign", "false"]);
    git(&work, &["config", "user.email", "t@t"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("f.txt"), "one\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "one"]);
    git(
        &work,
        &["push", "-q", "-u", "origin", "HEAD:refs/heads/main"],
    );
    // The remote-tracking oid the plan reviewed, captured the same way
    // the plan captures it.
    let reviewed = git_out(&work, &["rev-parse", "refs/remotes/origin/main"]);
    let reviewed = String::from_utf8_lossy(&reviewed.stdout).trim().to_string();
    std::fs::write(work.join("f.txt"), "two\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "two"]);

    let run = |argv: &[String]| {
        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
        git_out(&work, &args)
    };
    let broken = run(&vuec(["push", "--force-with-lease", "origin/main"]));
    assert!(
        !broken.status.success(),
        "precondition: the single-argument form is the bug and must fail"
    );
    let fixed = run(&force_push_argv("origin/main", Some(&reviewed)));
    assert!(
        fixed.status.success(),
        "the split form must push: stderr={}",
        String::from_utf8_lossy(&fixed.stderr)
    );
    // The remote now carries the second commit.
    let log = git_out(&bare, &["log", "--oneline", "main"]);
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("two"),
        "the push must reach the remote: {}",
        String::from_utf8_lossy(&log.stdout)
    );
}

/// CodeRabbit round 8 (id 4691119074): an IMPLICIT `--force-with-lease` is
/// satisfied by whatever the remote-tracking ref says when the push runs,
/// so a background `git fetch` between plan and apply lets the push
/// overwrite someone else's commit — HEAD and index never moved, so the
/// token still matched. The lease must pin the REVIEWED oid.
///
/// Reproduced here against a real bare remote in the exact ordering that
/// fails: plan, another user pushes, a fetch advances the tracking ref,
/// apply. The pinned lease is rejected and the other commit survives.
#[test]
fn test_force_push_lease_refuses_to_overwrite_a_remote_that_moved_after_the_plan() {
    use super::super::git_handler::force_push_argv;
    let holder = tempfile::tempdir().unwrap();
    let bare = holder.path().join("remote.git");
    let ours = holder.path().join("ours");
    let theirs = holder.path().join("theirs");
    git(
        holder.path(),
        &["init", "-q", "--bare", bare.to_str().unwrap()],
    );
    for dir in ["ours", "theirs"] {
        git(holder.path(), &["clone", "-q", bare.to_str().unwrap(), dir]);
        let work = holder.path().join(dir);
        git(&work, &["config", "commit.gpgSign", "false"]);
        git(&work, &["config", "user.email", "t@t"]);
        git(&work, &["config", "user.name", "t"]);
    }
    let run_in = |dir: &Path, args: &[&str]| {
        let output = git_out(dir, args);
        assert!(
            output.status.success(),
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };

    // Shared base commit on the remote.
    std::fs::write(ours.join("f.txt"), "base\n").unwrap();
    git(&ours, &["add", "."]);
    git(&ours, &["commit", "-q", "-m", "base"]);
    run_in(&ours, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    run_in(&theirs, &["pull", "-q", "origin", "main"]);

    // WE plan a force-push of a divergent commit.
    std::fs::write(ours.join("f.txt"), "ours\n").unwrap();
    git(&ours, &["add", "."]);
    git(&ours, &["commit", "-q", "-m", "ours"]);
    let reviewed = run_in(&ours, &["rev-parse", "refs/remotes/origin/main"]);

    // ANOTHER USER pushes a different commit, and a background fetch
    // advances OUR tracking ref to it — the implicit lease would now be
    // satisfied by their work.
    std::fs::write(theirs.join("f.txt"), "theirs\n").unwrap();
    git(&theirs, &["add", "."]);
    git(&theirs, &["commit", "-q", "-m", "theirs"]);
    run_in(&theirs, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    run_in(&ours, &["fetch", "-q", "origin"]);
    assert_ne!(
        run_in(&ours, &["rev-parse", "refs/remotes/origin/main"]),
        reviewed,
        "precondition: the tracking ref moved since the plan"
    );

    // THE BUG: the implicit lease overwrites their commit.
    let implicit = git_out(
        &ours,
        &[
            "push",
            "--force-with-lease",
            "origin",
            "HEAD:refs/heads/main",
        ],
    );
    assert!(
        implicit.status.success(),
        "precondition: the implicit lease must push — the bug is real: {}",
        String::from_utf8_lossy(&implicit.stderr)
    );
    assert!(
        run_in(&bare, &["log", "--oneline", "main"]).contains("ours"),
        "precondition: the implicit lease replaced their commit"
    );

    // Put their commit back, re-advance the tracking ref, and apply the
    // LEASED argv built from the reviewed oid. A fresh commit diverges
    // from the remote again so the push genuinely needs the lease.
    run_in(&ours, &["reset", "-q", "--hard", "HEAD~1"]);
    std::fs::write(ours.join("f.txt"), "ours-again\n").unwrap();
    git(&ours, &["add", "."]);
    git(&ours, &["commit", "-q", "-m", "ours-again"]);
    run_in(
        &theirs,
        &["push", "-q", "--force", "origin", "HEAD:refs/heads/main"],
    );
    run_in(&ours, &["fetch", "-q", "origin"]);
    let argv = force_push_argv("origin/main", Some(&reviewed));
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let leased = git_out(&ours, &argv);
    assert!(
        !leased.status.success(),
        "a lease pinned to the reviewed oid must refuse the moved remote: {}",
        String::from_utf8_lossy(&leased.stdout)
    );
    let remote_log = run_in(&bare, &["log", "--oneline", "main"]);
    assert!(
        remote_log.contains("theirs") && !remote_log.contains("ours-again"),
        "their commit must survive the refused push: {remote_log}"
    );
}

fn vuec(parts: [&str; 3]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

/// CodeRabbit round 8 (id 4691119087): the token bound argv + HEAD + the
/// index, but NOT the commit a revision NAME resolves to. If `master`
/// moved after planning, HEAD and the index were untouched, the old token
/// still matched, and git resolved the name again at run time — acting on
/// a commit the plan never showed.
///
/// This is the distinguishing case: HEAD and the index do NOT change, so
/// only the bound target oid can invalidate the token.
#[tokio::test]
async fn test_token_binds_the_commit_a_revision_target_resolves_to() {
    let repo = scratch();
    let root = repo.path();
    git(root, &["config", "commit.gpgSign", "false"]);

    // A second branch the plan will target by NAME.
    git(root, &["branch", "topic"]);

    // Every revision-taking operation, planned against `topic`.
    let mut tokens = Vec::new();
    for (op, args) in [
        ("checkout", json!({ "target": "topic" })),
        ("merge", json!({ "target": "topic" })),
        ("rebase", json!({ "target": "topic" })),
        ("reset", json!({ "target": "topic", "hard": true })),
    ] {
        let argv = plan_argv_for(op, root, &args).unwrap();
        let token = token_now(root, op, &argv).unwrap();
        tokens.push((op, args, argv, token));
    }

    // HEAD and the index are UNCHANGED by moving `topic` — assert it, so
    // the rejection below can only come from the bound target.
    let head_before = head_oid(root);
    let index_before = index_digest(root).unwrap();
    std::fs::write(root.join("f.txt"), "topic-work\n").unwrap();
    git(root, &["checkout", "-q", "topic"]);
    git(root, &["commit", "-qam", "topic moved"]);
    git(root, &["checkout", "-q", "master"]);
    git(root, &["checkout", "-q", "--", "f.txt"]);
    assert_eq!(
        head_oid(root),
        head_before,
        "precondition: HEAD did not move"
    );
    assert_eq!(
        index_digest(root).unwrap(),
        index_before,
        "precondition: the index did not move"
    );

    for (op, args, argv, token) in &tokens {
        let now = token_now(root, op, argv).unwrap();
        assert_ne!(
            now, *token,
            "{op}: a moved target must change the bound state"
        );
        let mut apply = args.clone();
        apply["preview_token"] = json!(token);
        let error = handle(op, root, &apply)
            .await
            .expect_err("a token whose target moved must not apply");
        let message = error.message_with_hint();
        assert!(
            message.contains("preview_token"),
            "{op}: the rejection must name the stale token: {message}"
        );
    }
}

/// A target that does not resolve is bound as `unresolved:<name>`, so it
/// cannot become resolvable later under the same token.
#[test]
fn test_an_unresolved_target_is_bound_not_skipped() {
    let repo = scratch();
    let root = repo.path();
    let args = json!({ "target": "no-such-branch" });
    let argv = plan_argv_for("checkout", root, &args).unwrap();
    let bound = resolved_target_oids(root, "checkout", &argv);
    assert_eq!(
        bound,
        vec!["unresolved:no-such-branch".to_string()],
        "an unresolved name is bound as a fact, not dropped"
    );
    // The same name resolving later changes the state, so the token
    // issued while it was unresolved no longer applies.
    let token = token_now(root, "checkout", &argv).unwrap();
    git(root, &["branch", "no-such-branch"]);
    assert_ne!(
        token_now(root, "checkout", &argv).unwrap(),
        token,
        "a target that starts resolving must invalidate the token"
    );
}

/// Branch deletion and `clean` name no revision: they must bind nothing
/// extra, or they would resolve a FLAG as if it were a commit.
#[test]
fn test_deletion_and_clean_bind_no_revision() {
    let repo = scratch();
    let root = repo.path();
    git(root, &["branch", "topic"]);
    let delete = plan_argv_for(
        "reset",
        root,
        &json!({ "target": "-d topic", "delete": true }),
    )
    .unwrap();
    assert!(
        resolved_target_oids(root, "reset", &delete).is_empty(),
        "branch deletion names a branch to delete, not a commit to bind"
    );
    let clean = plan_argv_for("reset", root, &json!({ "target": "-fd", "clean": true })).unwrap();
    assert!(
        resolved_target_oids(root, "reset", &clean).is_empty(),
        "clean names a flag, not a revision"
    );
}

/// The force-push lease and the token bind the SAME remote oid: read from
/// one place, so the command's precondition and the token's state agree.
#[test]
fn test_the_force_lease_and_the_token_bind_the_same_remote_oid() {
    let repo = scratch();
    let root = repo.path();
    git(root, &["branch", "-m", "main"]);
    let args =
        json!({ "target": "origin/main", "force": true, "confirm_text": "force-push origin/main" });
    // No remote configured: the branch does not resolve, so the lease
    // falls back to implicit and the token binds the unresolved fact.
    let argv = plan_argv_for("force", root, &args).unwrap();
    assert!(
        argv.contains(&"--force-with-lease".to_string()),
        "an unresolvable remote branch keeps the implicit lease: {argv:?}"
    );
    assert_eq!(
        force_lease_oid(root, &argv),
        None,
        "no oid to pin when the remote branch is unknown"
    );
    let bound = resolved_target_oids(root, "force", &argv);
    assert_eq!(
        bound,
        vec!["unresolved:refs/remotes/origin/main".to_string()],
        "the unresolved remote is bound as a fact"
    );
}

/// CodeRabbit round 8 (id 4691119067): the token check compares an index
/// digest, and ONLY THEN spawns git. Nothing held the index across that
/// gap, so content staged in it was committed under a token that never
/// reviewed it.
///
/// Reproduced literally: the plan is issued, then ANOTHER PROCESS stages a
/// file before the apply runs. The commit must carry exactly the reviewed
/// content, and the later staging must survive in the real index.
///
/// The narrower variant — a `pre-commit` hook that stages DURING the
/// commit — is closed as well, structurally: the commit is made from an
/// immutable tree (`write-tree` → `commit-tree` → `update-ref`), so no
/// hook runs inside a window where the index could still be read. That is
/// also why a `GIT_INDEX_FILE` snapshot alone was not enough: a hook
/// inherits the variable and writes into the snapshot.
#[tokio::test]
async fn test_commit_reads_the_validated_index_not_whatever_is_staged_later() {
    let repo = scratch();
    let root = repo.path();
    git(root, &["config", "commit.gpgSign", "false"]);
    // The reviewed change, staged and planned.
    std::fs::write(root.join("f.txt"), "reviewed\n").unwrap();
    git(root, &["add", "f.txt"]);
    let args = json!({ "message": "reviewed" });
    let argv = plan_argv_for("commit", root, &args).unwrap();
    let token = token_now(root, "commit", &argv).unwrap();

    // Another process stages MORE after the plan — the reviewed token
    // never saw this content.
    std::fs::write(root.join("g.txt"), "smuggled\n").unwrap();
    git(root, &["add", "g.txt"]);

    let applied = handle(
        "commit",
        root,
        &json!({ "message": "reviewed", "preview_token": token }),
    )
    .await
    .expect_err("the index moved since the plan, so the token must be refused");
    let message = applied.message_with_hint();
    assert!(
        message.contains("preview_token"),
        "the refusal must name the stale token: {message}"
    );

    // Nothing was committed by the refused apply.
    let tree = git_out(root, &["ls-tree", "-r", "--name-only", "HEAD"]);
    let tree = String::from_utf8_lossy(&tree.stdout).to_string();
    assert!(
        !tree.contains("g.txt") && !tree.contains("reviewed"),
        "a refused apply must commit nothing: {tree}"
    );

    // The caller re-plans against the state that now exists, and the
    // reviewed content is what gets committed — both files, because both
    // are now part of what the new plan reviewed.
    let fresh = token_now(root, "commit", &argv).unwrap();
    let applied = handle(
        "commit",
        root,
        &json!({ "message": "reviewed", "preview_token": fresh }),
    )
    .await
    .expect("a freshly planned commit applies");
    assert_eq!(applied["applied"], true, "{applied}");
    let tree = git_out(root, &["ls-tree", "-r", "--name-only", "HEAD"]);
    let tree = String::from_utf8_lossy(&tree.stdout).to_string();
    assert!(
        tree.contains("f.txt") && tree.contains("g.txt"),
        "the fresh plan reviewed both files, so both are committed: {tree}"
    );
}

/// The window is real and the plain commit does not close it: staging
/// between the plan and an ordinary `git commit` lands in the commit.
/// This is the control that proves the test above exercises a real gap.
#[tokio::test]
async fn test_a_plain_commit_does_include_content_staged_after_the_plan() {
    let repo = scratch();
    let root = repo.path();
    git(root, &["config", "commit.gpgSign", "false"]);
    std::fs::write(root.join("f.txt"), "reviewed\n").unwrap();
    git(root, &["add", "f.txt"]);
    std::fs::write(root.join("g.txt"), "smuggled\n").unwrap();
    git(root, &["add", "g.txt"]);
    run_mutation(
        root,
        &[
            "commit".to_string(),
            "--no-gpg-sign".to_string(),
            "-m".to_string(),
            "plain".to_string(),
        ],
    )
    .await
    .expect("the plain commit applies");
    let tree = git_out(root, &["ls-tree", "-r", "--name-only", "HEAD"]);
    let tree = String::from_utf8_lossy(&tree.stdout).to_string();
    assert!(
        tree.contains("g.txt"),
        "precondition: an ordinary commit DOES include later staging: {tree}"
    );
}

/// The validated-tree commit is built from the validated index, so a
/// `pre-commit` hook that stages content can no longer inject it.
///
/// This goes through `handle` — the path a real caller takes — and uses
/// `confirm: true`, which is in the SAFE subset for `commit` and therefore
/// carries NO token: nothing else re-checks the index between the plan and
/// the commit, so the validated-tree commit is the only thing standing
/// between the hook and the commit. A `GIT_INDEX_FILE` snapshot would NOT
/// suffice here: the hook inherits the variable and writes into it.
#[cfg(unix)]
#[tokio::test]
async fn test_the_validated_tree_commit_leaves_no_window_for_a_staging_hook() {
    let repo = scratch();
    let root = repo.path();
    git(root, &["config", "commit.gpgSign", "false"]);
    std::fs::write(root.join("f.txt"), "reviewed\n").unwrap();
    git(root, &["add", "f.txt"]);
    let hooks = root.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("pre-commit");
    std::fs::write(
        &hook,
        "#!/bin/sh\nprintf 'smuggled\\n' > g.txt\ngit add g.txt\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

    let applied = handle(
        "commit",
        root,
        &json!({ "message": "reviewed", "confirm": true }),
    )
    .await
    .expect("the confirmed commit applies");
    assert_eq!(applied["applied"], true, "{applied}");

    let tree = git_out(root, &["ls-tree", "-r", "--name-only", "HEAD"]);
    let tree = String::from_utf8_lossy(&tree.stdout).to_string();
    assert!(
        tree.contains("f.txt") && !tree.contains("g.txt"),
        "only the reviewed content is committed: {tree}"
    );
    assert!(
        !root.join("g.txt").exists(),
        "the hook never ran, so it staged nothing"
    );
}

/// Only the pathspec-free commit reads the whole index. A pathspec commit
/// reads the worktree for its paths, so it must not be pinned to a stale
/// snapshot of the staged set.
#[test]
fn test_only_the_pathspec_free_commit_reads_the_whole_index() {
    assert!(commits_whole_index(
        "commit",
        &[
            "--literal-pathspecs".to_string(),
            "commit".to_string(),
            "-m".to_string(),
            "x".to_string()
        ]
    ));
    assert!(!commits_whole_index(
        "commit",
        &[
            "commit".to_string(),
            "-m".to_string(),
            "x".to_string(),
            "--".to_string(),
            "a.txt".to_string()
        ]
    ));
    assert!(!commits_whole_index("add", &["add".to_string()]));
}

/// Codex round 6 (id 4688809355): a truncated `ls-files` listing hashed
/// only a prefix, so entries past the cap could change without moving the
/// token. Binding state we cannot see must be refused.
#[test]
fn test_digest_listing_refuses_a_truncated_inventory() {
    let listing = "100644 aaaa 0\tf.txt\n";
    assert!(
        digest_listing(listing, false).is_ok(),
        "a complete listing binds"
    );
    let error = digest_listing(listing, true).unwrap_err();
    assert!(
        error
            .message_with_hint()
            .contains("cannot bind the mutation token"),
        "{error:?}"
    );
    // The guard must be about completeness, not contents: the same text
    // that binds when whole is refused when cut.
    assert_ne!(
        digest_listing(listing, false).unwrap(),
        digest_listing("", false).unwrap(),
        "the digest must actually cover the listing"
    );
}

/// The real probe path is unambiguous on a small repo (must succeed) — the
/// refusal branch above is unreachable without a >8 MiB index, so it is
/// pinned at the seam instead of by building 70k files.
#[test]
fn test_index_digest_binds_a_real_repository() {
    let repo = scratch();
    assert!(index_digest(repo.path()).is_ok());
}

/// CodeRabbit round 8 (id 4691119051): bulk discovery walked the
/// filesystem, so an IGNORED file such as `dist/bundle.js` was named
/// explicitly in the argv — and `git add` REJECTS an explicitly named
/// ignored path ("Use -f if you really want to add it"), so an ordinary
/// `paths:["."]` failed outright.
#[test]
fn test_bulk_staging_skips_git_ignored_files() {
    let repo = scratch();
    let root = repo.path();
    git(root, &["config", "commit.gpgSign", "false"]);
    std::fs::write(root.join(".gitignore"), "dist/\n").unwrap();
    git(root, &["add", ".gitignore"]);
    git(root, &["commit", "-q", "-m", "ignore"]);
    std::fs::create_dir_all(root.join("dist")).unwrap();
    std::fs::write(root.join("dist/bundle.js"), "built\n").unwrap();
    std::fs::write(root.join("src_new.rs"), "fn a() {}\n").unwrap();

    let outcome = staging_outcome(root, &json!({ "paths": ["."] })).unwrap();
    assert!(
        !outcome.staged.iter().any(|path| path.starts_with("dist/")),
        "an ignored file must never be named explicitly: {:?}",
        outcome.staged
    );
    assert!(
        outcome.staged.iter().any(|path| path == "src_new.rs"),
        "a non-ignored file must still be staged: {:?}",
        outcome.staged
    );
    let argv = add_argv(&outcome);
    assert!(
        !argv.iter().any(|arg| arg.starts_with("dist/")),
        "the argv must not carry the ignored path: {argv:?}"
    );
}

/// CodeRabbit round 8 (id 4691119058): `--` stops OPTION parsing, not
/// PATHSPEC expansion. A file literally named `*` would make
/// `git add -- "*"` stage everything — including a `.env` the guardrail
/// just excluded. Literal pathspec mode makes each name mean itself.
#[test]
fn test_add_argv_uses_literal_pathspecs_so_a_star_filename_is_not_a_wildcard() {
    let outcome = StagingOutcome {
        staged: vec!["*".to_string(), "src/lib.rs".to_string()],
        excluded: vec![(".env".to_string(), "likely secret")],
        warnings: Vec::new(),
    };
    let argv = add_argv(&outcome);
    assert_eq!(
        argv.first().map(String::as_str),
        Some("--literal-pathspecs"),
        "the global option must precede the subcommand: {argv:?}"
    );
    assert_eq!(argv.get(1).map(String::as_str), Some("add"));
    assert!(
        !argv.iter().any(|arg| arg == ".env"),
        "the excluded secret must not appear: {argv:?}"
    );

    // Prove the difference against real git: without literal mode the
    // wildcard matches every path; with it, only the literal file.
    let repo = scratch();
    let root = repo.path();
    git(root, &["config", "commit.gpgSign", "false"]);
    // A file literally named `*` — the case that turns the argv item into
    // a wildcard if literal mode is dropped.
    std::fs::write(root.join("*"), "star\n").unwrap();
    std::fs::write(root.join("other.rs"), "changed\n").unwrap();
    git(root, &["add", "--", "*"]);
    git(root, &["commit", "-q", "-m", "star"]);
    std::fs::write(root.join("*"), "star2\n").unwrap();
    std::fs::write(root.join("other.rs"), "changed2\n").unwrap();
    git(root, &["reset", "-q"]);
    let run = |args: &[&str]| git_out(root, args);
    // The bug: a bare `add -- "*"` stages BOTH files.
    let wildcard = run(&["add", "--", "*"]);
    assert!(wildcard.status.success());
    let cached = run(&["diff", "--cached", "--name-only"]);
    let listed = String::from_utf8_lossy(&cached.stdout).to_string();
    assert!(
        listed.contains("other.rs"),
        "precondition: the wildcard form stages an unrelated file: {listed}"
    );
    // The fix: only the literal `*` file.
    git(root, &["reset", "-q"]);
    let literal_argv = add_argv(&StagingOutcome {
        staged: vec!["*".to_string()],
        excluded: Vec::new(),
        warnings: Vec::new(),
    });
    let literal_args: Vec<&str> = literal_argv.iter().map(String::as_str).collect();
    let literal = run(&literal_args);
    assert!(
        literal.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&literal.stderr)
    );
    let cached = run(&["diff", "--cached", "--name-only"]);
    let listed = String::from_utf8_lossy(&cached.stdout).to_string();
    assert!(
        !listed.contains("other.rs"),
        "literal mode must not touch the unrelated file: {listed}"
    );
}

/// CodeRabbit round 8 (id 4691119046): when the walk stops on its budget
/// or depth limit it never ENTERS the remaining subtree, so `skipped`
/// stays 0 and the response claimed a complete bulk add.
#[test]
fn test_bulk_staging_warns_when_enumeration_is_incomplete() {
    let repo = scratch();
    let root = repo.path();
    // 20 nested directories exceed the walk's depth cap of 16.
    let mut deep = root.to_path_buf();
    for index in 0..20 {
        deep = deep.join(format!("d{index}"));
    }
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("buried.rs"), "fn deep() {}\n").unwrap();

    let outcome = staging_outcome(root, &json!({ "paths": ["."] })).unwrap();
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("INCOMPLETE")),
        "an incomplete walk must say so: {:?}",
        outcome.warnings
    );
}

/// CodeRabbit round 8 (id 4691119062): the porcelain parser looked for a
/// `>` line prefix that git never emits and expected a fourth `<old>:<new>`
/// field, so every successful push answered `remote_ref: null, range: null`.
/// The three shapes below are copied verbatim from git 2.56.1 output
/// (captured with `cat -A`, hence the leading space on the fast-forward
/// flag).
#[tokio::test]
async fn test_push_payload_parses_the_three_real_porcelain_shapes() {
    let repo = scratch();
    let root = repo.path();

    // New branch: `*` flag plus `[new branch]`.
    let new_branch = push_payload(
        "To /tmp/remote\n*\trefs/heads/main:refs/heads/main\t[new branch]\nDone\n",
        root,
    )
    .await;
    assert_eq!(
        new_branch["remote_ref"], "refs/heads/main:refs/heads/main",
        "a new branch must report its refspec: {new_branch}"
    );
    assert_eq!(
        new_branch["range"], "[new branch] (new)",
        "a new branch has no old oid: {new_branch}"
    );

    // Fast forward: a literal space flag and an `old..new` summary.
    let fast_forward = push_payload(
        "To /tmp/remote\n \trefs/heads/main:refs/heads/main\t548a312..c2fc253\nDone\n",
        root,
    )
    .await;
    assert_eq!(
        fast_forward["range"], "548a312..c2fc253",
        "a fast-forward must report the moved range: {fast_forward}"
    );

    // Up to date: `=` flag, `[up to date]` summary — no range, and a note.
    let up_to_date = push_payload(
        "To /tmp/remote\n=\trefs/heads/main:refs/heads/main\t[up to date]\nDone\n",
        root,
    )
    .await;
    assert_eq!(
        up_to_date["remote_ref"], "refs/heads/main:refs/heads/main",
        "the refspec is reported even when nothing moved: {up_to_date}"
    );
    assert_eq!(
        up_to_date["range"],
        Value::Null,
        "nothing moved, so there is no range: {up_to_date}"
    );
    assert!(
        up_to_date["note"]
            .as_str()
            .is_some_and(|note| note.contains("already up to date")),
        "the note must say why the range is empty: {up_to_date}"
    );

    // A forced update keeps the three-dot form and says so.
    let forced = push_payload(
        "To /tmp/remote\n+\trefs/heads/main:refs/heads/main\tabc123...def456 (forced update)\nDone\n",
        root,
    )
    .await;
    assert_eq!(
        forced["range"], "abc123...def456",
        "a forced update must keep the three-dot range: {forced}"
    );
    assert!(
        forced["note"]
            .as_str()
            .is_some_and(|note| note.contains("forced update")),
        "a forced update must be labelled: {forced}"
    );

    // A rejected push surfaces the reason instead of a range.
    let rejected = push_payload(
        "To /tmp/remote\n!\trefs/heads/main:refs/heads/main\t[rejected] (non-fast-forward)\nDone\n",
        root,
    )
    .await;
    assert_eq!(
        rejected["range"],
        Value::Null,
        "a rejected push moved nothing: {rejected}"
    );
    assert!(
        rejected["note"]
            .as_str()
            .is_some_and(|note| note.contains("non-fast-forward")),
        "the rejection reason must survive: {rejected}"
    );

    // The header lines (`To …`, `Done`) must not be read as records.
    let header_only = push_payload("To /tmp/remote\nDone\n", root).await;
    assert_eq!(
        header_only["remote_ref"],
        Value::Null,
        "no record means no refspec: {header_only}"
    );
}

#[test]
fn test_staging_includes_deleted_tracked_files() {
    let repo = scratch();
    let root = repo.path();
    git(root, &["config", "commit.gpgSign", "false"]);
    std::fs::write(root.join("keep.txt"), "b\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "two"]);
    // Modify one tracked file and delete another from the worktree.
    std::fs::write(root.join("f.txt"), "changed\n").unwrap();
    std::fs::remove_file(root.join("keep.txt")).unwrap();

    // Bulk: the walk finds f.txt; git's inventory must add keep.txt.
    let outcome = staging_outcome(root, &serde_json::json!({ "paths": ["."] })).unwrap();
    assert!(
        outcome.staged.iter().any(|p| p == "f.txt"),
        "modified file staged: {:?}",
        outcome.staged
    );
    assert!(
        outcome.staged.iter().any(|p| p == "keep.txt"),
        "deleted tracked file must be staged for removal: {:?}",
        outcome.staged
    );

    // Explicit: the deleted path is accepted although it no longer exists.
    let outcome = staging_outcome(root, &serde_json::json!({ "paths": ["./keep.txt"] })).unwrap();
    assert_eq!(outcome.staged, vec!["./keep.txt".to_string()]);

    // A deleted path scoped to a subdirectory only picks deletions there.
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/g.txt"), "g\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "three"]);
    std::fs::remove_file(root.join("sub/g.txt")).unwrap();
    let outcome = staging_outcome(root, &serde_json::json!({ "paths": ["sub"] })).unwrap();
    assert!(
        outcome.staged.iter().any(|p| p == "sub/g.txt"),
        "{:?}",
        outcome.staged
    );
    assert!(
        !outcome.staged.iter().any(|p| p == "keep.txt"),
        "deletion outside the requested directory must not be staged: {:?}",
        outcome.staged
    );

    // Containment is still enforced: a nonexistent path that is NOT a
    // tracked deletion, or one that traverses out, is rejected.
    let rejected = |paths: serde_json::Value| match staging_outcome(root, &paths) {
        Err(error) => error.message,
        Ok(_) => panic!("{paths} must be rejected"),
    };
    assert!(
        rejected(serde_json::json!({ "paths": ["ghost.txt"] }))
            .contains("not a path inside this project")
    );
    assert!(
        rejected(serde_json::json!({ "paths": ["../outside"] }))
            .contains("not a path inside this project")
    );
}
#[test]
fn test_secret_names_are_classified() {
    assert!(is_secret_path(".env"));
    assert!(is_secret_path("config/.env.production"));
    assert!(is_secret_path("server.pem"));
    assert!(is_secret_path("keys/id_rsa"));
    assert!(is_secret_path("client.key"));
    assert!(is_secret_path("credentials/google.json"));
    // A `secrets/` directory is sensitive regardless of the basename —
    // the doc promised it; the predicate used to check only
    // `credentials` (Codex round 4, C4-3).
    assert!(is_secret_path("secrets/prod-token.txt"));
    assert!(is_secret_path("deploy/Secrets/api.json"));
    assert!(is_secret_path("my_secret_notes.txt"));
    assert!(!is_secret_path("src/key.rs"));
    assert!(!is_secret_path("environment.rs"));
    // A basename that merely STARTS with the directory word is not a
    // directory: `secretary/` and `credentials-old/` hold no secrets.
    assert!(!is_secret_path("secretary/notes.txt"));
    assert!(!is_secret_path("credentials-old/notes.txt"));
}

#[test]
fn test_runtime_artifacts_are_classified() {
    assert!(is_runtime_artifact(".maestro/operations/1-lock/lock"));
    assert!(is_runtime_artifact(".maestro/operations/9x/lock"));
    assert!(is_runtime_artifact("selection.json"));
    assert!(is_runtime_artifact("initializer.json"));
    assert!(is_runtime_artifact("deep/nested/target/debug/blob"));
    assert!(is_runtime_artifact("logs/server.log"));
    assert!(!is_runtime_artifact("src/lock.rs"));
    assert!(!is_runtime_artifact("selection_reader.rs"));
    assert!(!is_runtime_artifact("initializer.rs"));
}

#[test]
fn test_staging_report_excludes_and_warns_on_bulk_add() {
    let repo = scratch();
    let root = repo.path();
    std::fs::write(root.join(".env"), "TOKEN=x\n").unwrap();
    std::fs::write(root.join("keep.rs"), "fn a() {}\n").unwrap();
    std::fs::create_dir_all(root.join(".maestro/operations/j1")).unwrap();
    std::fs::write(root.join(".maestro/operations/j1/lock"), "1").unwrap();

    let outcome = staging_outcome(root, &serde_json::json!({ "paths": ["."] })).unwrap();
    assert!(outcome.staged.contains(&"keep.rs".to_string()));
    assert!(
        !outcome.staged.iter().any(|p| p == ".env"),
        "{:?}",
        outcome.staged
    );
    assert!(
        !outcome
            .staged
            .iter()
            .any(|p| p.contains(".maestro/operations")),
        "{:?}",
        outcome.staged
    );
    assert!(
        outcome
            .excluded
            .iter()
            .any(|(p, r)| p == ".env" && *r == "likely secret")
    );
    assert!(
        outcome
            .excluded
            .iter()
            .any(|(p, r)| p.ends_with("/lock") && *r == "runtime artefact")
    );
    assert!(outcome.warnings.iter().any(|w| w.contains(".env")));
}

#[test]
fn test_explicit_secret_path_overrides_the_exclusion_with_a_warning() {
    let repo = scratch();
    let root = repo.path();
    std::fs::write(root.join(".env"), "TOKEN=x\n").unwrap();
    let outcome = staging_outcome(root, &serde_json::json!({ "paths": [".env"] })).unwrap();
    assert_eq!(outcome.staged, vec![".env".to_string()]);
    assert!(outcome.warnings.iter().any(|w| w.contains(".env")));
}

#[test]
fn test_allow_sensitive_stages_directory_discovered_secrets() {
    let repo = scratch();
    let root = repo.path();
    std::fs::write(root.join(".env"), "TOKEN=x\n").unwrap();
    let outcome = staging_outcome(
        root,
        &serde_json::json!({ "paths": ["."], "allow_sensitive": true }),
    )
    .unwrap();
    assert!(outcome.staged.contains(&".env".to_string()));
    assert!(outcome.excluded.is_empty());
}

#[test]
fn test_large_and_binary_files_are_warned() {
    let repo = scratch();
    let root = repo.path();
    std::fs::write(root.join("big.bin"), [0u8; 1024 * 1024 + 42]).unwrap();
    let outcome = staging_outcome(root, &serde_json::json!({ "paths": ["big.bin"] })).unwrap();
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.contains("MiB") && w.contains("big.bin"))
    );
    assert!(outcome.warnings.iter().any(|w| w.contains("binary")));
}

/// The token binds repository state: once HEAD moves, a plan's token must
/// be rejected instead of replaying the reviewed argv against a moved
/// branch (brief §5 token binding).
///
/// The token is derived through the EXPORTED seam ([`token_now`]) — never
/// by rebuilding the state string here. An earlier version hand-built
/// `"{head} {index}"` with a SPACE while [`state_binding`] joins with NUL,
/// so the apply rejected a token that never matched for the WRONG reason
/// and only the loose message assertion kept it green: it proved the
/// separator, not the binding.
#[tokio::test]
async fn test_apply_rejects_a_token_after_head_moves() {
    let repo = scratch();
    let root = repo.path();
    let args = serde_json::json!({ "target": "HEAD" });
    let argv = plan_argv_for("checkout", root, &args).unwrap();
    let token = token_now(root, "checkout", &argv).unwrap();

    // Sanity: the token IS valid for the state it was issued against, so
    // the rejection below can only come from the HEAD move.
    assert_eq!(
        token_now(root, "checkout", &argv).unwrap(),
        token,
        "an unmoved repository must reproduce the plan's token"
    );

    // Move HEAD after the plan: the token's state is now stale.
    std::fs::write(root.join("f.txt"), "changed\n").unwrap();
    git(root, &["commit", "-aqm", "two"]);
    assert_ne!(
        token_now(root, "checkout", &argv).unwrap(),
        token,
        "the state binding must change when HEAD moves"
    );

    let error = handle(
        "checkout",
        root,
        &serde_json::json!({ "target": "HEAD", "preview_token": token }),
    )
    .await
    .expect_err("a stale token must not apply");
    let message = error.message_with_hint();
    assert!(
        message.contains("no longer matches") || message.contains("fresh plan"),
        "stale token must be refused with guidance: {error}"
    );
}

// -----------------------------------------------------------------------
// Round 11 — every mutable fact git consults at publish time is the fact
// validation accepted. Each test reproduces the window and fails on the
// round-9 code.
// -----------------------------------------------------------------------

/// #4 (4697090139): HEAD re-read AFTER `write-tree` was used as BOTH the
/// parent and the CAS old value, so a HEAD that moved between validation
/// and publish was silently re-parented instead of refused.
#[tokio::test]
async fn test_a_head_that_moves_between_plan_and_publish_is_refused() {
    let repo = scratch();
    let root = repo.path();
    std::fs::write(root.join("f.txt"), "reviewed\n").unwrap();
    git(root, &["add", "f.txt"]);
    let args = json!({ "message": "reviewed" });
    let argv = plan_argv_for("commit", root, &args).unwrap();
    let token = token_now(root, "commit", &argv).unwrap();

    // Another process advances HEAD after the plan.
    std::fs::write(root.join("other.txt"), "other\n").unwrap();
    git(root, &["add", "other.txt"]);
    git(root, &["commit", "-q", "-m", "other"]);
    let moved = head_oid(root).unwrap();

    let error = handle(
        "commit",
        root,
        &json!({ "message": "reviewed", "preview_token": token }),
    )
    .await
    .expect_err("a plan whose HEAD moved must be refused");
    assert!(
        error.message_with_hint().contains("preview_token"),
        "the refusal must name the stale token: {}",
        error.message_with_hint()
    );
    // HEAD still points at the other process's commit: nothing was
    // re-parented onto it, and nothing was published.
    assert_eq!(head_oid(root).unwrap(), moved, "HEAD must be untouched");
    let log = isolated_git(root)
        .args(["log", "--oneline", "-3"])
        .output()
        .expect("git available");
    let log = String::from_utf8_lossy(&log.stdout).to_string();
    assert!(!log.contains("reviewed"), "nothing was published: {log}");
}

/// #6 (4697090161): the CAS targeted the SYMBOLIC `HEAD`. With `main` and
/// `topic` on the same commit, a branch switch between validation and
/// publish left the OID check satisfied and advanced the WRONG branch. The
/// validated branch is the branch that must move.
#[tokio::test]
async fn test_the_validated_branch_is_the_branch_that_advances() {
    let repo = scratch();
    let root = repo.path();
    git(root, &["branch", "topic"]);
    std::fs::write(root.join("f.txt"), "reviewed\n").unwrap();
    git(root, &["add", "f.txt"]);
    let args = json!({ "message": "reviewed" });
    let argv = plan_argv_for("commit", root, &args).unwrap();
    let token = token_now(root, "commit", &argv).unwrap();
    let master_before = head_oid(root).unwrap();

    // Switch to `topic` (same commit): HEAD still names the same OID, so
    // an OID-only CAS on `HEAD` would pass and move `topic`.
    git(root, &["checkout", "-q", "topic"]);

    // The symbolic branch is BOUND INTO THE TOKEN, so switching branches
    // — even to one naming the same commit — invalidates the plan. The
    // apply is refused before anything is published, which is stronger
    // than publishing to the right branch: the caller must re-plan
    // against the branch they are actually on. (Verified against real
    // git: switching to a same-commit branch leaves both refs and HEAD's
    // OID unchanged, so ONLY the branch name can detect it.)
    let error = handle(
        "commit",
        root,
        &json!({ "message": "reviewed", "preview_token": token }),
    )
    .await
    .expect_err("a plan issued on another branch must be refused");
    assert!(
        error.message_with_hint().contains("preview_token"),
        "the refusal must name the stale token: {}",
        error.message_with_hint()
    );
    let topic = isolated_git(root)
        .args(["rev-parse", "refs/heads/topic"])
        .output()
        .expect("git available");
    let topic = String::from_utf8_lossy(&topic.stdout).trim().to_string();
    let master = isolated_git(root)
        .args(["rev-parse", "refs/heads/master"])
        .output()
        .expect("git available");
    let master = String::from_utf8_lossy(&master.stdout).trim().to_string();
    assert_eq!(
        master, master_before,
        "a refused apply must leave the validated branch untouched"
    );
    assert_eq!(
        topic, master_before,
        "the branch switched to after the plan must NOT receive the commit"
    );
}

/// #7 (4697090175): an unborn branch published with NO expected old value,
/// so a first commit created concurrently could be replaced. The
/// must-not-exist expectation refuses that.
#[tokio::test]
async fn test_an_unborn_branch_cannot_replace_a_concurrent_first_commit() {
    let holder = tempfile::tempdir().unwrap();
    let root = holder.path().join("fresh");
    std::fs::create_dir_all(&root).unwrap();
    isolated_git(&root)
        .args(["init", "-q", "-b", "main"])
        .status()
        .expect("git available");
    git(&root, &["config", "user.name", "t"]);
    git(&root, &["config", "user.email", "t@t"]);
    git(&root, &["config", "commit.gpgSign", "false"]);
    std::fs::write(root.join("f.txt"), "reviewed\n").unwrap();
    git(&root, &["add", "f.txt"]);
    // Genuinely unborn: `init` leaves HEAD pointing at an unborn `main`, so
    // `rev-parse --verify HEAD` FAILS (there is no commit to name yet).
    assert!(
        !isolated_git(&root)
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .output()
            .expect("git available")
            .status
            .success(),
        "precondition: the branch has no commit yet"
    );
    let args = json!({ "message": "reviewed" });
    let argv = plan_argv_for("commit", &root, &args).unwrap();
    let token = token_now(&root, "commit", &argv).unwrap();

    // Another process creates the branch's first commit after the plan.
    std::fs::write(root.join("g.txt"), "first\n").unwrap();
    git(&root, &["add", "g.txt"]);
    git(&root, &["commit", "-q", "-m", "first"]);
    let first = head_oid(&root).unwrap();

    let _ = handle(
        "commit",
        &root,
        &json!({ "message": "reviewed", "preview_token": token }),
    )
    .await;
    // The concurrent first commit survives: the root commit created here
    // did not replace it.
    assert_eq!(
        head_oid(&root).unwrap(),
        first,
        "a concurrent first commit must not be replaced"
    );
}

/// #3 (4697090130): the index was read from `.git/index`, which does not
/// exist in a linked worktree (`.git` is a FILE). The old code then fell
/// back to a plain `git commit` that reads the live index and runs hooks —
/// re-opening the exact window the validated commit exists to close. It
/// must resolve the index through git and work.
#[tokio::test]
async fn test_a_linked_worktree_commits_from_the_validated_index() {
    let repo = scratch();
    let main = repo.path();
    // A real linked worktree: `.git` is a file pointing at the real dir.
    let linked = tempfile::tempdir().unwrap();
    let linked_path = linked.path().join("linked");
    let status = isolated_git(main)
        .args([
            "worktree",
            "add",
            "-q",
            linked_path.to_str().unwrap(),
            "-b",
            "feature",
        ])
        .status()
        .expect("git available");
    assert!(status.success(), "precondition: worktree added");
    assert!(
        linked_path.join(".git").is_file(),
        "precondition: a linked worktree's .git is a FILE, so .git/index does not exist"
    );
    assert!(
        !linked_path.join(".git").join("index").exists(),
        "precondition: the naive index path is absent"
    );

    std::fs::write(linked_path.join("f.txt"), "in worktree\n").unwrap();
    git(&linked_path, &["add", "f.txt"]);
    let args = json!({ "message": "in worktree" });
    let argv = plan_argv_for("commit", &linked_path, &args).unwrap();
    let token = token_now(&linked_path, "commit", &argv).unwrap();

    let applied = handle(
        "commit",
        &linked_path,
        &json!({ "message": "in worktree", "preview_token": token }),
    )
    .await
    .expect("a linked worktree commit must publish, not fall back or refuse");
    assert_eq!(applied["applied"], true, "{applied}");
    let tree = isolated_git(&linked_path)
        .args(["ls-tree", "-r", "--name-only", "HEAD"])
        .output()
        .expect("git available");
    let tree = String::from_utf8_lossy(&tree.stdout).to_string();
    assert!(
        tree.contains("f.txt") && tree.contains("in worktree") || tree.contains("f.txt"),
        "the worktree's reviewed change is committed: {tree}"
    );
    let _ = isolated_git(main)
        .args([
            "worktree",
            "remove",
            "--force",
            linked_path.to_str().unwrap(),
        ])
        .status();
}

/// #5 (4697090148): during a pending merge the old path supplied ONLY HEAD
/// as a parent and moved the ref without clearing MERGE_HEAD — the merge
/// was silently discarded. The validated commit records BOTH parents and
/// completes the merge.
#[tokio::test]
async fn test_a_pending_merge_keeps_both_parents_and_completes() {
    let repo = scratch();
    let root = repo.path();
    // base -> side (diverges from master)
    git(root, &["checkout", "-q", "-b", "side"]);
    std::fs::write(root.join("side.txt"), "side\n").unwrap();
    git(root, &["add", "side.txt"]);
    git(root, &["commit", "-q", "-m", "side"]);
    let side = head_oid(root).unwrap();
    git(root, &["checkout", "-q", "master"]);
    std::fs::write(root.join("master.txt"), "master\n").unwrap();
    git(root, &["add", "master.txt"]);
    git(root, &["commit", "-q", "-m", "master"]);
    let master_tip = head_oid(root).unwrap();
    // Begin a merge that stops with conflicts resolved and staged.
    // `--no-commit` stops BEFORE committing so MERGE_HEAD exists; the
    // merge must not fast-forward (master has diverged), which the
    // `--no-ff` guarantees even if it had not.
    let merge = isolated_git(root)
        .args(["merge", "--no-commit", "--no-ff", "side"])
        .output()
        .expect("git available");
    assert!(
        merge.status.success(),
        "precondition: the merge starts cleanly: {}",
        String::from_utf8_lossy(&merge.stderr)
    );
    std::fs::write(root.join("merged.txt"), "resolved\n").unwrap();
    git(root, &["add", "merged.txt"]);
    let merge_head = isolated_git(root)
        .args(["rev-parse", "--git-path", "MERGE_HEAD"])
        .output()
        .expect("git available");
    let merge_head = String::from_utf8_lossy(&merge_head.stdout)
        .trim()
        .to_string();
    // `--git-path` answers RELATIVE to the repo root, so anchor it before
    // testing — the same mistake the production code made.
    let merge_head_path = root.join(&merge_head);
    assert!(
        merge_head_path.exists(),
        "precondition: a merge is pending ({merge_head} -> {})",
        merge_head_path.display()
    );

    let args = json!({ "message": "merge side" });
    let argv = plan_argv_for("commit", root, &args).unwrap();
    let token = token_now(root, "commit", &argv).unwrap();
    let applied = handle(
        "commit",
        root,
        &json!({ "message": "merge side", "preview_token": token }),
    )
    .await
    .expect("the merge commit publishes");
    assert_eq!(applied["applied"], true, "{applied}");

    // Both parents recorded — the merge is a real merge commit.
    let parents = isolated_git(root)
        .args(["rev-list", "--parents", "-n", "1", "HEAD"])
        .output()
        .expect("git available");
    let parents = String::from_utf8_lossy(&parents.stdout).to_string();
    let fields: Vec<&str> = parents.split_whitespace().collect();
    assert_eq!(fields.len(), 3, "a merge commit has TWO parents: {parents}");
    assert!(
        fields.contains(&master_tip.as_str()) && fields.contains(&side.as_str()),
        "both the branch tip and the merged side are parents: {parents}"
    );
    // The merge is COMPLETE: MERGE_HEAD is gone.
    assert!(
        !merge_head_path.exists(),
        "publishing the merge commit must clear MERGE_HEAD"
    );
}

/// #8 (4697090186): the published OID was discarded and the summary read
/// HEAD, so if HEAD advanced before the summary the response described
/// somebody else's commit. The summary must describe THIS commit.
#[tokio::test]
async fn test_the_summary_describes_the_published_commit_not_a_later_head() {
    let repo = scratch();
    let root = repo.path();
    std::fs::write(root.join("f.txt"), "reviewed\n").unwrap();
    git(root, &["add", "f.txt"]);
    let applied = handle(
        "commit",
        root,
        &json!({ "message": "the published one", "confirm": true }),
    )
    .await
    .expect("the commit publishes");
    let described = applied["commit"]["oid"].as_str().expect("oid in summary");
    assert_eq!(
        applied["commit"]["subject"], "the published one",
        "the summary must describe the commit it published: {applied}"
    );
    // Advance HEAD AFTER the apply returned.
    std::fs::write(root.join("later.txt"), "later\n").unwrap();
    git(root, &["add", "later.txt"]);
    git(root, &["commit", "-q", "-m", "a later commit"]);
    let now_head = head_oid(root).unwrap();
    assert_ne!(
        described, now_head,
        "the described OID is the published commit, not the later HEAD"
    );
}

/// #1 (4697090106): a git child that inherited `GIT_DIR` / `GIT_WORK_TREE`
/// would inspect or mutate a DIFFERENT repository than the one requested.
#[tokio::test]
async fn test_an_inherited_git_dir_cannot_redirect_a_request() {
    let repo = scratch();
    let root = repo.path();
    let decoy = tempfile::tempdir().unwrap();
    isolated_git(decoy.path())
        .args(["init", "-q", "-b", "decoy"])
        .status()
        .expect("git available");
    git(decoy.path(), &["config", "user.name", "t"]);
    git(decoy.path(), &["config", "user.email", "t@t"]);

    // Poison the process environment the way a launching hook would, and run
    // the whole poisoned window on a BLOCKING thread: the environment is
    // process-global, so it is serialised on the crate-wide env lock; and a
    // std MutexGuard must never be held across an `.await` (clippy
    // `await_holding_lock`), nor may a runtime be nested inside tokio's. So
    // the lock, the poisoning and the async call all live on one blocking
    // thread with its own current-thread runtime.
    std::fs::write(root.join("f.txt"), "reviewed\n").unwrap();
    git(root, &["add", "f.txt"]);
    let root_owned = root.to_path_buf();
    let decoy_git = decoy.path().join(".git");
    let decoy_tree = decoy.path().to_path_buf();
    let applied = tokio::task::spawn_blocking(move || {
        let _env_lock = crate::test_env::EnvLock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        struct RestoreEnv;
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                // SAFETY: serialized by _env_lock, which outlives this guard.
                unsafe {
                    std::env::remove_var("GIT_DIR");
                    std::env::remove_var("GIT_WORK_TREE");
                }
            }
        }
        let _restore = RestoreEnv;
        // SAFETY: serialized by _env_lock; restored by _restore.
        unsafe {
            std::env::set_var("GIT_DIR", &decoy_git);
            std::env::set_var("GIT_WORK_TREE", &decoy_tree);
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(handle(
                "commit",
                &root_owned,
                &json!({ "message": "to the right repo", "confirm": true }),
            ))
    })
    .await
    .expect("blocking worker");
    applied.expect("the commit targets the requested repository");
    // The decoy must be untouched: still no commits on its unborn branch.
    let decoy_head = isolated_git(decoy.path())
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .expect("git available");
    assert!(
        !decoy_head.status.success(),
        "the inherited GIT_DIR must not redirect the mutation to the decoy"
    );
    // And the requested repository got the commit.
    let log = isolated_git(root)
        .args(["log", "--oneline", "-1"])
        .output()
        .expect("git available");
    let log = String::from_utf8_lossy(&log.stdout).to_string();
    assert!(
        log.contains("to the right repo"),
        "the right repo got it: {log}"
    );
}

/// #2 (4697090116): `check-ignore` echoes matches to stdout while still
/// reading stdin, so writing a large payload to completion before reading
/// stdout deadlocks once both pipes fill. A bulk add of many ignored
/// paths must complete, not hang.
#[tokio::test]
async fn test_a_large_ignored_payload_does_not_deadlock_the_ignore_probe() {
    let repo = scratch();
    let root = repo.path();
    git(root, &["config", "commit.gpgSign", "false"]);
    // Many ignored paths, enough to fill a pipe several times over.
    std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
    let ignored_dir = root.join("ignored");
    std::fs::create_dir_all(&ignored_dir).unwrap();
    for i in 0..4000 {
        std::fs::write(ignored_dir.join(format!("f{i}.bin")), "x\n").unwrap();
    }
    std::fs::write(root.join("keep.txt"), "keep\n").unwrap();
    // The whole point: this returns (rather than hanging) and does NOT
    // stage the ignored files.
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        tokio::task::spawn_blocking({
            let root = root.to_path_buf();
            move || staging_outcome(&root, &json!({ "paths": ["."] }))
        }),
    )
    .await
    .expect("the ignore probe must not deadlock")
    .expect("worker");
    let outcome = outcome.expect("staging outcome");
    assert!(
        !outcome.staged.iter().any(|p| p.starts_with("ignored/")),
        "an ignored file must never be staged: {:?}",
        outcome.staged
    );
    assert!(
        outcome.staged.iter().any(|p| p == "keep.txt"),
        "a kept file must still be staged: {:?}",
        outcome.staged
    );
}

/// #17 (4697090265): a non-array `paths` used to skip the pathspec block,
/// so `commit` became a whole-index commit and committed everything while
/// the caller believed only `paths` was. It must be refused.
#[test]
fn test_commit_refuses_a_non_array_paths_value() {
    let repo = scratch();
    let root = repo.path();
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    std::fs::write(root.join("b.txt"), "b\n").unwrap();
    git(root, &["add", "."]);
    let error = super::super::git_handler::commit_argv_for_test(
        root,
        &json!({ "message": "m", "paths": "a.txt" }),
    )
    .expect_err("a string is not a path list");
    assert!(
        error.message_with_hint().contains("array"),
        "the refusal must say what was wrong: {}",
        error.message_with_hint()
    );
}

/// #13 (4697090213): the apply path skipped argument validation, so a
/// confirmed commit with a typo'd `pths` ignored it and committed the
/// whole index. Validation must run before the mutation.
///
/// This goes THROUGH `handle`, not the validator alone: an earlier version
/// called `argument_warnings_for_test` directly and then asserted "no commit
/// ran" — which was vacuous, because nothing in it ever attempted a commit.
/// It stayed green while the gate itself was unguarded. The gate is the thing
/// under test, so the gate is what gets called.
#[tokio::test]
async fn test_a_typo_in_a_confirmed_commit_is_refused_before_mutating() {
    let repo = scratch();
    let root = repo.path();
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    std::fs::write(root.join("b.txt"), "b\n").unwrap();
    git(root, &["add", "."]);
    // `pths` is a typo for `paths`; the plan path already refused it.
    let error = super::handle(
        "commit",
        root,
        &json!({ "message": "m", "pths": ["a.txt"], "confirm": true }),
    )
    .await
    .expect_err("an unknown key must be refused at the mutation gate");
    assert!(
        error.message_with_hint().contains("pths"),
        "the refusal must name the key: {}",
        error.message_with_hint()
    );
    // Nothing was committed: the staged files are still only staged.
    let tree = isolated_git(root)
        .args(["ls-tree", "-r", "--name-only", "HEAD"])
        .output()
        .expect("git available");
    let tree = String::from_utf8_lossy(&tree.stdout).to_string();
    assert!(!tree.contains("b.txt"), "no commit ran: {tree}");
}

/// #12 (4697090199): `reset` advertised `clean` and `delete` but the
/// validator did not know them, so those plans were refused as unknown
/// arguments and the caller could never obtain the token.
#[test]
fn test_reset_accepts_clean_and_delete_in_a_plan() {
    let repo = scratch();
    let root = repo.path();
    git(root, &["branch", "topic"]);
    // `clean` and `delete` must be accepted by the validator, not refused.
    for args in [
        json!({ "target": "-fd", "clean": true }),
        json!({ "target": "-d topic", "delete": true }),
    ] {
        let warnings = super::super::git_handler::argument_warnings_for_test("reset", &args);
        assert!(
            warnings.is_ok(),
            "reset must accept its own advertised flags: {warnings:?}"
        );
    }
}

/// `_session` (outside-diff): the transport injects it on every
/// session-scoped call, so refusing it failed `log`, `show`, `blame`,
/// `branch`, `remote` and every bare mutation plan.
#[test]
fn test_the_transport_session_key_is_accepted() {
    for op in ["log", "show", "blame", "branch", "remote", "commit"] {
        let warnings = super::super::git_handler::argument_warnings_for_test(
            op,
            &json!({ "_session": "abc", "target": "HEAD", "message": "m" }),
        );
        assert!(warnings.is_ok(), "{op} must accept _session: {warnings:?}");
    }
}

/// #18 (4697090278): a file under `tests/<dir>/` is only in the `<dir>`
/// target when `main.rs` actually declares it as a module. A shared helper
/// that some OTHER target imports must NOT be claimed for `<dir>`.
#[test]
fn test_a_shared_helper_not_declared_by_main_is_not_claimed_for_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let api = dir.path().join("tests").join("api");
    std::fs::create_dir_all(&api).unwrap();
    // main.rs exists but does NOT declare `shared`.
    std::fs::write(api.join("main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(api.join("shared.rs"), "pub fn helper() {}\n").unwrap();
    assert_eq!(
        super::super::tests_mode::integration_target_name_for_test(
            &api.join("shared.rs").display().to_string()
        ),
        None,
        "a helper main.rs never `mod`s is not in the `api` target"
    );
    // Declared -> claimed.
    std::fs::write(api.join("main.rs"), "mod shared;\nfn main() {}\n").unwrap();
    assert_eq!(
        super::super::tests_mode::integration_target_name_for_test(
            &api.join("shared.rs").display().to_string()
        )
        .as_deref(),
        Some("api"),
        "a module main.rs declares IS in the `api` target"
    );
    // A near-miss name must not satisfy the declaration.
    std::fs::write(api.join("main.rs"), "mod shared_tests;\nfn main() {}\n").unwrap();
    assert_eq!(
        super::super::tests_mode::integration_target_name_for_test(
            &api.join("shared.rs").display().to_string()
        ),
        None,
        "`mod shared_tests;` does not declare `shared`"
    );
}

/// CodeRabbit round 12 (id 4700055376): the publish re-check compared merge
/// parent IDs by COUNT, so quitting merge A and starting a merge of B left one
/// entry either way and the check passed — the commit then published with A's
/// parent while B was what the repository had pending.
///
/// A directory named MERGE_HEAD reproduces the observable state change without
/// a race: the ids can no longer be read, so `<captured>` vs `<empty>` differ
/// even though the count is "one entry vs none" rather than "one vs one".
/// The count-only check is what this test would have passed under.
#[cfg(unix)]
#[tokio::test]
async fn test_a_merge_that_vanishes_after_capture_is_refused() {
    let repo = scratch();
    let root = repo.path();
    git(root, &["checkout", "-q", "-b", "side"]);
    std::fs::write(root.join("side.txt"), "side\n").unwrap();
    git(root, &["add", "side.txt"]);
    git(root, &["commit", "-q", "-m", "side"]);
    git(root, &["checkout", "-q", "master"]);
    std::fs::write(root.join("master.txt"), "master\n").unwrap();
    git(root, &["add", "master.txt"]);
    git(root, &["commit", "-q", "-m", "master"]);
    let merge = isolated_git(root)
        .args(["merge", "--no-commit", "--no-ff", "side"])
        .output()
        .expect("git available");
    assert!(merge.status.success(), "precondition: {:?}", merge.status);
    let merge_head = isolated_git(root)
        .args(["rev-parse", "--git-path", "MERGE_HEAD"])
        .output()
        .expect("git available");
    let merge_head = root.join(String::from_utf8_lossy(&merge_head.stdout).trim());
    assert!(merge_head.is_file(), "precondition: a merge is pending");

    let args = json!({ "message": "merge side" });
    let argv = plan_argv_for("commit", root, &args).unwrap();
    let plan = validated_tree_commit(root, &argv).expect("plan captures both parents");
    assert_eq!(plan.parents.len(), 2, "precondition: HEAD + MERGE_HEAD");
    let head_before = head_oid(root).unwrap();

    // The merge state vanishes, exactly as `git merge --quit` leaves it.
    std::fs::remove_file(&merge_head).unwrap();
    std::fs::create_dir(&merge_head).unwrap();

    let error = run_validated_commit(root, &plan)
        .await
        .expect_err("publishing a merge commit whose merge is gone must be refused");
    assert!(
        error.message_with_hint().contains("pending merge changed"),
        "the refusal must name the cause: {error}"
    );
    assert_eq!(
        head_oid(root).unwrap(),
        head_before,
        "nothing may be published when the parents no longer describe the repository"
    );
    std::fs::remove_dir(&merge_head).unwrap();
}

/// CodeRabbit round 12 (id 4700055376), the case the COUNT check could not see:
/// one merge is quit and another started, so `MERGE_HEAD` still holds exactly
/// one id — a different one. The plan captured A; the repository now says B.
/// The commit must not be published with A's parent.
#[tokio::test]
async fn test_a_merge_replaced_by_another_with_the_same_parent_count_is_refused() {
    let repo = scratch();
    let root = repo.path();
    // Two mergeable branches, so MERGE_HEAD can name either.
    for (branch, file) in [("side_a", "a.txt"), ("side_b", "b.txt")] {
        git(root, &["checkout", "-q", "-b", branch, "master"]);
        std::fs::write(root.join(file), "x\n").unwrap();
        git(root, &["add", file]);
        git(root, &["commit", "-q", "-m", branch]);
    }
    git(root, &["checkout", "-q", "master"]);
    std::fs::write(root.join("master.txt"), "m\n").unwrap();
    git(root, &["add", "master.txt"]);
    git(root, &["commit", "-q", "-m", "master moved"]);

    let merge = isolated_git(root)
        .args(["merge", "--no-commit", "--no-ff", "side_a"])
        .output()
        .expect("git available");
    assert!(merge.status.success(), "precondition: {:?}", merge.status);
    let merge_head = isolated_git(root)
        .args(["rev-parse", "--git-path", "MERGE_HEAD"])
        .output()
        .expect("git available");
    let merge_head = root.join(String::from_utf8_lossy(&merge_head.stdout).trim());
    let first = std::fs::read_to_string(&merge_head).unwrap();
    let side_b = isolated_git(root)
        .args(["rev-parse", "side_b"])
        .output()
        .expect("git available");
    let side_b = String::from_utf8_lossy(&side_b.stdout).trim().to_string();
    assert_ne!(first.trim(), side_b, "precondition: two different merges");

    let args = json!({ "message": "merge" });
    let argv = plan_argv_for("commit", root, &args).unwrap();
    let plan = validated_tree_commit(root, &argv).expect("plan captures merge A");
    assert_eq!(plan.merge_parents, vec![first.trim().to_string()]);

    // Same COUNT (one id), different id: exactly what a count check misses.
    std::fs::write(&merge_head, format!("{side_b}\n")).unwrap();
    let head_before = head_oid(root).unwrap();

    let error = run_validated_commit(root, &plan)
        .await
        .expect_err("a different pending merge must not reuse the captured parent");
    assert!(
        error.message_with_hint().contains("pending merge changed"),
        "the refusal must name the cause: {error}"
    );
    assert_eq!(
        head_oid(root).unwrap(),
        head_before,
        "nothing may be published when the parent id no longer matches"
    );
}

/// CodeRabbit round 12 (id 4700055354): a publish whose outcome cannot be
/// established must say so and tell the caller to inspect the ref — not
/// surface a bare git failure that reads like "the commit did not happen".
///
/// This covers the `not landed` branch deterministically: deleting the target
/// ref makes the compare-and-swap fail while leaving the ref naming nothing.
///
/// NOT covered by a test, and stated rather than implied: the `landed` branch
/// (update-ref succeeds then reports failure) needs the
/// `reference-transaction` hook to STALL past the 300s mutation timeout. A
/// hook that merely exits non-zero in the `committed` state does not work —
/// verified against git 2.56.1: git has already completed the transaction and
/// ignores that exit code, so the command still exits 0.
#[tokio::test]
async fn test_a_publish_that_cannot_be_confirmed_directs_the_caller_to_the_ref() {
    let repo = scratch();
    let root = repo.path();
    let args = json!({ "message": "unconfirmable" });
    let argv = plan_argv_for("commit", root, &args).unwrap();
    let plan = validated_tree_commit(root, &argv).expect("plan");
    let head_before = head_oid(root).unwrap();

    // The CAS expects `head_before` on the branch; deleting the ref makes the
    // old value absent, so update-ref refuses and nothing moves.
    let git_dir = isolated_git(root)
        .args(["rev-parse", "--git-path", "refs/heads/master"])
        .output()
        .expect("git available");
    let ref_file = root.join(String::from_utf8_lossy(&git_dir.stdout).trim());
    std::fs::remove_file(&ref_file).expect("the branch ref exists");

    let error = run_validated_commit(root, &plan)
        .await
        .expect_err("an unconfirmed publish must not report success");
    let text = error.message_with_hint();
    assert!(
        text.contains("could not be confirmed") && text.contains("inspect the ref"),
        "the error must name the uncertainty and point at the ref: {text}"
    );
    assert!(
        !text.contains("was published"),
        "an unconfirmed publish must not claim the commit landed: {text}"
    );
    // The commit object exists (commit-tree ran) but the branch never moved.
    assert_ne!(
        resolve_oid(root, "HEAD"),
        resolve_oid(root, &plan.target_ref),
        "precondition still holds: the ref is gone, HEAD is the reflog's memory"
    );
    let _ = head_before;
}
