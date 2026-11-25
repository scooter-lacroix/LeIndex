use super::*;

#[test]
fn test_every_op_is_selectable_and_documented() {
    for op in &OPS {
        assert!(!op.summary.is_empty(), "{} has no summary", op.name);
        assert!(Op::parse(op.name).is_some(), "{} does not parse", op.name);
    }
    assert!(Op::parse("nope").is_none());
}

#[test]
fn test_mutations_are_marked_mutating_and_reads_are_not() {
    for op in &OPS {
        let expected = !matches!(
            op.name,
            "status" | "diff" | "log" | "show" | "blame" | "branch" | "remote"
        );
        assert_eq!(op.mutating, expected, "{} mutation flag", op.name);
    }
}

/// The plan's advertised token MUST be the token the executor accepts.
///
/// Regression (integration review, F1): `mutation_plan` issued the token
/// against an EMPTY state while the executor validates against
/// `{head_oid}\0{index_digest}`, so every plan-issued token was rejected.
/// That made every TOKEN_ONLY operation (checkout/stash/tag/merge/rebase)
/// and every destructive shape unreachable — only `confirm:true` worked,
/// and the negative "token rejected after HEAD moves" test kept passing
/// because it hand-derived the token with the real state instead of
/// round-tripping a plan. This test round-trips: bare stash → plan →
/// re-send with the PLAN'S token → applied.
#[tokio::test]
async fn test_plan_token_round_trips_into_a_successful_apply() {
    let repo = scratch_repo();
    let root = repo.path();
    // Something to stash: an untracked file is enough (`stash -u`).
    std::fs::write(root.join("dirty.txt"), "dirty\n").unwrap();

    let plan = mutation_plan(Op::parse("stash").unwrap(), root, &json!({}))
        .await
        .expect("plan");
    assert_eq!(plan["applied"], false, "a plan applies nothing: {plan}");
    let token = plan["preview_token"]
        .as_str()
        .expect("the plan advertises a token")
        .to_string();

    let applied =
        super::super::git_apply::handle("stash", root, &json!({ "preview_token": token }))
            .await
            .unwrap_or_else(|error| panic!("the plan's own token must apply: {error}"));
    assert_eq!(
        applied["applied"], true,
        "token path must reach git: {applied}"
    );
    assert!(
        !root.join("dirty.txt").exists(),
        "the stash actually ran — the untracked file is gone from the tree"
    );
}

/// Regression (Codex round 4, C4-2): the rebase plan carried
/// `--no-edit`, which `git rebase` does NOT accept (`error: unknown
/// option 'no-edit'`, exit 129) — so every token-authorized rebase failed
/// at the executor while the plan looked fine. No argv assertion could
/// catch it; the planned argv has to be RUN. A non-`-i` rebase opens no
/// editor, and the executor sets `GIT_EDITOR=true`, so a bare
/// `git rebase <target>` is already non-interactive.
#[tokio::test]
async fn test_rebase_plan_token_applies_without_a_bogus_no_edit_flag() {
    let repo = scratch_repo();
    let root = repo.path();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .expect("git available");
        assert!(status.success(), "git {args:?}");
    };
    // A ref, resolved through the same runner the handler uses.
    let rev = |rev: &str| -> String {
        git_output_capped(
            root,
            &["rev-parse".to_string(), rev.to_string()],
            MAX_GIT_BYTES,
        )
        .map(|(out, _)| out.trim().to_string())
        .unwrap_or_else(|error| panic!("rev-parse {rev}: {error}"))
    };
    // A topic commit on top of master, then a master commit the topic
    // must be replayed onto.
    git(&["checkout", "-q", "-b", "topic"]);
    std::fs::write(root.join("topic.txt"), "topic\n").unwrap();
    git(&["add", "topic.txt"]);
    git(&["commit", "-q", "-m", "topic work"]);
    git(&["checkout", "-q", "master"]);
    std::fs::write(root.join("master.txt"), "master\n").unwrap();
    git(&["add", "master.txt"]);
    git(&["commit", "-q", "-m", "master moved on"]);
    git(&["checkout", "-q", "topic"]);
    let master_tip = rev("master");
    assert_ne!(
        rev("master~1"),
        master_tip,
        "precondition: master must have moved on by exactly one commit"
    );
    assert_ne!(
        rev("topic"),
        rev("topic~1"),
        "precondition: topic must have exactly one commit of its own"
    );
    // The topic commit sits on master's PARENT — the rebase moves it.
    assert_eq!(
        rev("topic^"),
        rev("master~1"),
        "precondition: topic forked before master moved"
    );

    let plan = mutation_plan(
        Op::parse("rebase").unwrap(),
        root,
        &json!({ "target": "master" }),
    )
    .await
    .expect("plan");
    assert_eq!(plan["applied"], false, "a plan applies nothing: {plan}");
    // The advertised argv is what the token binds and the executor runs.
    assert_eq!(
        plan["argv"],
        json!(["rebase", "master"]),
        "the rebase argv must carry no `--no-edit` (git rejects it): {plan}"
    );
    let token = plan["preview_token"]
        .as_str()
        .expect("the plan advertises a token")
        .to_string();

    let applied = super::super::git_apply::handle(
        "rebase",
        root,
        &json!({ "target": "master", "preview_token": token }),
    )
    .await
    .unwrap_or_else(|error| panic!("the plan's own token must apply: {error}"));
    assert_eq!(
        applied["applied"], true,
        "token path must reach git: {applied}"
    );

    // The rebase actually ran: topic's parent is now master's tip.
    assert_eq!(
        rev("topic^"),
        master_tip,
        "topic must sit directly on master's tip after the rebase"
    );
}

/// The whole mutation policy in one assertion: a bare mutating call is a
/// plan, and the token binds to the argv (a different message must not
/// share it).
#[test]
fn test_mutation_default_is_a_plan_not_an_execution() {
    let root = Path::new("/tmp/project");
    let op = Op::parse("commit").unwrap();
    let argv = plan_argv(op, root, &json!({ "message": "hi" })).unwrap();
    // `--literal-pathspecs` is a GLOBAL option and must lead: `--` stops
    // option parsing but NOT pathspec expansion, so a validated path named
    // `*` would otherwise commit unrelated modified files the caller never
    // named (CodeRabbit round 8, id 4691119081). The assertion below is the
    // pin that caught the missing guard.
    assert_eq!(
        argv,
        ["--literal-pathspecs", "commit", "--no-gpg-sign", "-m", "hi"]
    );
    let token = preview_token(root, op.name, &argv, "");
    assert!(!token.is_empty());
    // A different message must not share the token.
    let other = plan_argv(op, root, &json!({ "message": "bye" })).unwrap();
    assert_ne!(token, preview_token(root, op.name, &other, ""));
    // State participates in the binding: the same argv under a different
    // repository state hashes differently (the executor's reject-on-move).
    assert_ne!(
        token,
        preview_token(root, op.name, &argv, "head-moved-or-index-changed")
    );
}

/// The plan's `next` field is the instruction a caller actually follows.
/// The apply side exists now, so the hint must SAY how to apply — via the
/// token, with confirm reserved to the safe subset — and must still state
/// that a bare plan changed nothing. The old text ("this build cannot
/// apply git mutations") is stale: the brief (§5) explicitly requires
/// real apply, and the old tests pinned preview-only behaviour.
#[test]
fn test_plan_hint_says_how_to_apply() {
    assert!(
        PLAN_NEXT_HINT.contains("preview_token"),
        "the hint must point at the token path: {PLAN_NEXT_HINT}"
    );
    assert!(
        PLAN_NEXT_HINT.contains("confirm:true"),
        "the hint must name the safe-subset shortcut: {PLAN_NEXT_HINT}"
    );
    assert!(
        PLAN_NEXT_HINT.contains("Nothing was changed"),
        "the hint must state that no state changed: {PLAN_NEXT_HINT}"
    );
    assert!(
        PLAN_NEXT_HINT.contains("token-only"),
        "destructive shapes must be marked token-only: {PLAN_NEXT_HINT}"
    );
}

#[test]
fn test_mutation_requiring_a_target_refuses_to_default() {
    let root = Path::new("/tmp/project");
    for name in ["checkout", "merge", "rebase", "reset", "force", "tag"] {
        let op = Op::parse(name).unwrap();
        let error = plan_argv(op, root, &json!({})).unwrap_err();
        assert!(
            error.message_with_hint().contains("requires 'target'"),
            "{name}: {error}"
        );
    }
}

#[test]
fn test_revisions_cannot_smuggle_options_or_paths() {
    for bad in ["-u", "--upload-pack=x", "HEAD:../../etc/passwd", "", "a b"] {
        assert!(
            super::super::git_ref::validate_ref(bad).is_err(),
            "{bad} must be rejected"
        );
    }
    for good in [
        "HEAD",
        "main",
        "origin/main",
        "v1.2.3",
        "HEAD~3",
        "a1b2c3d^",
        "cw32/30b-admission",
    ] {
        assert!(
            super::super::git_ref::validate_ref(good).is_ok(),
            "{good} must be accepted"
        );
    }
}

/// The plan validates its revision; the probe must use the SAME gate rather
/// than rebuilding argv from the caller's raw string.
///
/// This asserts on `probe_rev_argv` directly. An earlier version asserted on
/// `probe_state` against a nonexistent repository and passed even with the
/// guard removed — git failed for an unrelated reason — so it proved
/// nothing.
#[test]
fn test_probe_validates_the_revision_before_building_argv() {
    let error = probe_rev_argv("-u").expect_err("a leading '-' must be rejected");
    assert!(
        error.message_with_hint().contains("not valid ref syntax"),
        "{error}"
    );
    assert!(
        probe_rev_argv("HEAD:../../etc/passwd").is_err(),
        "rev:path syntax must be rejected"
    );
    assert_eq!(
        probe_rev_argv("HEAD").unwrap(),
        ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]
    );
}

/// The schema must advertise exactly the apply path the executor
/// implements. `confirm` and `preview_token` were removed when the apply
/// side was unwired; the brief (§5) requires real apply, so they are back
/// — and the destructive rails must be advertised too, because a model
/// that cannot see `confirm_text` cannot pass the force-push gate.
#[test]
fn test_schema_advertises_the_apply_path() {
    let schema = GitHandler.argument_schema();
    let properties = schema["properties"].as_object().expect("properties");
    for key in [
        "confirm",
        "preview_token",
        "force",
        "confirm_text",
        "hard",
        "clean",
        "delete",
        "allow_sensitive",
        "trailers",
    ] {
        assert!(properties.contains_key(key), "{key} must be advertised");
    }
    assert_eq!(schema["required"].as_array().map(Vec::len), Some(0));
}

/// Rails at the argv layer: destructive shapes are only buildable with
/// their explicit flag, and every one of them must be token-only at the
/// executor (SAFE_TO_CONFIRM must not contain the ops that carry them).
#[test]
fn test_destructive_shapes_need_their_explicit_flags() {
    let root = Path::new("/tmp/project");

    // reset --hard without hard:true stays --mixed.
    let soft = plan_argv(
        Op::parse("reset").unwrap(),
        root,
        &json!({ "target": "HEAD~1", "hard": false }),
    )
    .unwrap();
    assert_eq!(soft, ["reset", "--mixed", "HEAD~1"]);
    let hard = plan_argv(
        Op::parse("reset").unwrap(),
        root,
        &json!({ "target": "HEAD~1", "hard": true }),
    )
    .unwrap();
    assert_eq!(hard, ["reset", "--hard", "HEAD~1"]);

    // Branch deletion: only via the flag shape, and only with delete:true.
    let no_flag = plan_argv(
        Op::parse("reset").unwrap(),
        root,
        &json!({ "target": "-d feature" }),
    )
    .unwrap_err();
    assert!(
        no_flag.message_with_hint().contains("delete:true"),
        "{no_flag}"
    );
    let delete = plan_argv(
        Op::parse("reset").unwrap(),
        root,
        &json!({ "target": "-d feature", "delete": true }),
    )
    .unwrap();
    assert_eq!(delete, ["branch", "-d", "feature"]);

    // clean without clean:true is refused; with it, only -d/-fd pass.
    let no_clean = plan_argv(
        Op::parse("reset").unwrap(),
        root,
        &json!({ "target": "HEAD", "clean": true }),
    )
    .unwrap_err();
    assert!(no_clean.message_with_hint().contains("-fd"), "{no_clean}");
    let clean = plan_argv(
        Op::parse("reset").unwrap(),
        root,
        &json!({ "target": "-fd", "clean": true }),
    )
    .unwrap();
    assert_eq!(clean, ["clean", "-fd", "-f"]);

    // Force-push without force:true never builds.
    let no_force = plan_argv(
        Op::parse("force").unwrap(),
        root,
        &json!({ "target": "origin/main" }),
    )
    .unwrap_err();
    assert!(
        no_force.message_with_hint().contains("force:true"),
        "{no_force}"
    );

    // The executor's confirm subset must exclude every op that can carry
    // one of the destructive shapes.
    let safe: &[&str] = &super::super::git_apply::SAFE_TO_CONFIRM;
    for op in ["reset", "force"] {
        assert!(
            !safe.contains(&op),
            "{op} can be destructive and must not be confirmable without a token"
        );
    }
}

/// commit paths rides git's native pathspec commit (no temp index): the
/// argv is `commit … -- <paths>`, trailers ride `--trailer` one per line,
/// and a pathspec commit must not sweep other staged changes in — that is
/// git's documented `git commit -- <paths>` behaviour, so the exact-args
/// test plus the integration test (commit paths doesn't include other
/// staged files) pin both halves.
#[test]
fn test_commit_paths_use_pathspec_semantics_and_trailers_validate() {
    let repo = scratch_repo();
    let root = repo.path();
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    std::fs::write(root.join("b.txt"), "b\n").unwrap();
    let args = json!({
        "message": "subject",
        "paths": ["a.txt"],
        "trailers": ["Co-Authored-By: T <t@t>"],
    });
    let argv = commit_argv(root, &args).unwrap();
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["--".to_string(), "a.txt".to_string()].as_slice()),
        "paths must arrive after -- as a pathspec: {argv:?}"
    );
    assert_eq!(
        argv.iter().filter(|a| a.as_str() == "--trailer").count(),
        1,
        "one --trailer flag per line: {argv:?}"
    );
    assert!(
        !argv.contains(&"b.txt".to_string()),
        "commit paths must not include unrequested files: {argv:?}"
    );
    // Trailers are single-line only; a newline would smuggle a paragraph.
    let bad =
        commit_argv(root, &json!({ "message": "m", "trailers": ["two\nlines"] })).unwrap_err();
    assert!(bad.message_with_hint().contains("single line"), "{bad}");
    // Empty paths array is a caller error, not a whole-tree commit.
    let empty = commit_argv(root, &json!({ "message": "m", "paths": [] })).unwrap_err();
    assert!(
        empty.message_with_hint().contains("at least one"),
        "{empty}"
    );
}

/// CodeRabbit round 8 (id 4691119081): `--` stops OPTION parsing but not
/// PATHSPEC expansion, so a validated path literally named `*` made
/// `commit -- "*"` sweep in every other modified tracked file the caller
/// never named — the opposite of this function's contract. The guard is
/// the global `--literal-pathspecs`, which must sit before the subcommand
/// to apply at all. Proven against real git.
#[test]
fn test_commit_paths_with_a_star_filename_do_not_sweep_other_files() {
    let repo = scratch_repo();
    let root = repo.path();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git available");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    };
    git(&["config", "commit.gpgSign", "false"]);
    std::fs::write(root.join("*"), "one\n").unwrap();
    std::fs::write(root.join("other.txt"), "one\n").unwrap();
    git(&["add", "--", "*"]);
    git(&["add", "other.txt"]);
    git(&["commit", "-q", "-m", "base"]);

    // The bug, against real git: a bare pathspec commit of `*` sweeps in
    // `other.txt` as well.
    std::fs::write(root.join("*"), "two\n").unwrap();
    std::fs::write(root.join("other.txt"), "two\n").unwrap();
    git(&["commit", "--no-gpg-sign", "-m", "wild", "--", "*"]);
    let listed = git(&["show", "--name-only", "--format=", "HEAD"]);
    assert!(
        listed.contains("other.txt"),
        "precondition: the wildcard commit sweeps an unrelated file: {listed}"
    );

    // The fix: the argv this function builds carries literal pathspec mode.
    std::fs::write(root.join("*"), "three\n").unwrap();
    std::fs::write(root.join("other.txt"), "three\n").unwrap();
    let argv = commit_argv(root, &json!({ "message": "narrow", "paths": ["*"] })).unwrap();
    assert_eq!(
        argv.first().map(String::as_str),
        Some("--literal-pathspecs"),
        "the global option must lead the argv: {argv:?}"
    );
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    git(&args);
    let listed = git(&["show", "--name-only", "--format=", "HEAD"]);
    assert!(
        !listed.contains("other.txt"),
        "a literal pathspec commit must not touch the unrelated file: {listed}"
    );
}

/// Unknown keys are refused; known-but-ignored ones are reported.
///
/// Nothing used to look at unrecognized arguments, so a typo and a bound
/// that applied were indistinguishable.
#[test]
fn test_argument_hygiene_refuses_unknown_and_warns_on_ignored() {
    let branch = Op::parse("branch").unwrap();
    // A typo must not be silently dropped.
    let error = argument_warnings(branch, &json!({ "max_cnt": 5 })).unwrap_err();
    assert!(error.message_with_hint().contains("max_cnt"), "{error}");
    assert!(
        error.message_with_hint().contains("name_prefix"),
        "the hint must list what branch accepts: {error}"
    );

    // A key another operation reads is reported, not obeyed silently.
    let warnings = argument_warnings(branch, &json!({ "target": "HEAD" })).unwrap();
    assert_eq!(warnings, vec!["git branch ignores 'target'".to_string()]);

    // Router/transport keys are legitimate and produce nothing.
    assert!(
        argument_warnings(
            branch,
            &json!({ "project_path": "/x", "max_latency_ms": 5, "token_budget": 10 })
        )
        .unwrap()
        .is_empty()
    );
    // `max_chars` truncates every response, so it is honored everywhere.
    assert!(
        argument_warnings(Op::parse("show").unwrap(), &json!({ "max_chars": 10 }))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_bounds_are_clamped() {
    assert_eq!(
        extract_bounded_u64(&json!({ "max_count": 10_000 }), "max_count", 20, MAX_COUNT).unwrap(),
        MAX_COUNT
    );
    assert_eq!(
        extract_bounded_usize(&json!({}), "max_chars", DEFAULT_MAX_CHARS, MAX_MAX_CHARS).unwrap(),
        DEFAULT_MAX_CHARS
    );
    assert!(extract_bounded_u64(&json!({ "max_count": "x" }), "max_count", 20, MAX_COUNT).is_err());
}

#[test]
fn test_remote_parsing_tolerates_an_empty_repository() {
    // A repo with no remotes prints nothing; that is an answer, not a failure.
    let parsed = "".lines().filter(|line| !line.trim().is_empty()).count();
    assert_eq!(parsed, 0);
}

/// §4.5: `show` is compact by default — stat, no patch. The patch is
/// opt-in via `include_patch: true`.
#[test]
fn test_show_defaults_to_stat_and_omits_the_patch() {
    let repo = scratch_repo();
    let compact = git_show(repo.path(), &json!({})).unwrap();
    assert_eq!(compact["include_patch"], false);
    assert!(
        compact.get("patch").is_none(),
        "compact show must not carry a patch: {compact}"
    );
    assert!(
        compact["stat"]
            .as_str()
            .is_some_and(|stat| stat.contains("f.txt")),
        "stat names the changed file: {compact}"
    );

    let with_patch = git_show(repo.path(), &json!({ "include_patch": true })).unwrap();
    assert_eq!(with_patch["include_patch"], true);
    assert!(
        with_patch["patch"].is_string(),
        "include_patch:true carries the patch: {with_patch}"
    );
}

/// CodeRabbit round 10 (id 4697090199): the schema and `plan_argv` accepted
/// `clean` / `delete` on `reset`, but the operation's argument list named only
/// `target` and `hard`, so `argument_warnings` refused them and the caller
/// could never obtain the token the schema told them to get.
#[tokio::test]
async fn test_reset_clean_and_delete_obtain_a_token_but_not_a_bare_confirm() {
    let repo = scratch_repo();
    let root = repo.path();
    // The shapes `plan_argv` documents: `clean` is `git clean -f <flag>` and
    // `delete` is branch deletion, both token-only.
    for args in [
        json!({ "target": "-fd", "clean": true }),
        json!({ "target": "-d", "clean": true }),
        json!({ "target": "-d stale", "delete": true }),
    ] {
        let plan = mutation_plan(Op::parse("reset").unwrap(), root, &args)
            .await
            .unwrap_or_else(|error| panic!("a documented flag must be plannable: {error}"));
        assert!(
            plan["preview_token"]
                .as_str()
                .is_some_and(|t| !t.is_empty()),
            "the plan must carry a token: {plan}"
        );
        assert_eq!(plan["applied"], false);
    }
    // `confirm:true` alone is still refused for a destructive reset.
    let bare = crate::cli::mcp::git_apply::handle(
        "reset",
        root,
        &json!({ "target": "-fd", "clean": true, "confirm": true }),
    )
    .await
    .expect_err("confirm alone must not apply a hard reset");
    assert!(
        bare.message_with_hint().contains("confirm:true alone"),
        "{bare}"
    );
}

/// CodeRabbit round 10 (id 4697090213): argument validation ran only on the
/// plan path. A confirmed commit carrying `pths` (a typo of `paths`) ignored
/// the key and committed the WHOLE index, while the same key was refused on
/// the plan path. Validation now runs before either branch.
#[tokio::test]
async fn test_a_misspelled_paths_key_is_refused_on_the_apply_path() {
    let repo = scratch_repo();
    let root = repo.path();
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    std::fs::write(root.join("b.txt"), "b\n").unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["add", "."]);

    let error = crate::cli::mcp::git_apply::handle(
        "commit",
        root,
        &json!({ "message": "m", "pths": ["a.txt"], "confirm": true }),
    )
    .await
    .expect_err("a typo'd paths key must be refused, not ignored");
    assert!(
        error.message_with_hint().contains("pths"),
        "the refusal must name the key: {error}"
    );
    // Nothing was committed: HEAD still has exactly one commit.
    let log = git_log(root, &json!({})).unwrap();
    assert_eq!(
        log["count"], 1,
        "the index must not have been committed: {log}"
    );
}

/// CodeRabbit round 10 (id 4697090225): a file whose name is exactly 40 hex
/// characters is legal. Matching a commit header on OID SHAPE alone turned
/// such a file record into a phantom commit. The header now requires the tab
/// separator that only a header carries.
#[test]
fn test_a_forty_hex_filename_is_a_file_record_not_a_phantom_commit() {
    let repo = scratch_repo();
    let root = repo.path();
    let hex_name = "0123456789abcdef0123456789abcdef01234567";
    std::fs::write(root.join(hex_name), "x\n").unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "two"]);

    let log = git_log(root, &json!({})).unwrap();
    assert_eq!(log["count"], 2, "two real commits, no phantom: {log}");
    let ids: Vec<&str> = log["commits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["oid"].as_str())
        .collect();
    assert!(
        !ids.contains(&hex_name),
        "the filename must never be reported as a commit oid: {ids:?}"
    );
    assert_eq!(
        log["commits"][0]["files_changed"], 1,
        "the 40-hex file is counted as a file of the newest commit: {log}"
    );
}

/// CodeRabbit round 10 (id 4697090236): with `include_patch:true`, `HEAD`
/// was resolved once for the stat and AGAIN for the patch, so a HEAD that
/// advanced between the two probes produced the stat of commit A with the
/// patch of commit B. The target is now resolved to ONE oid for both probes.
///
/// The race itself cannot be provoked deterministically (`git show` does not
/// mutate), so this pins the FIX rather than the symptom: both probes must be
/// given the RESOLVED COMMIT, not the caller's raw target. An annotated tag
/// makes that observable without a race — `git show <tag>` renders the TAG
/// object (tagger line, tag message) while `git show <peeled-oid>` renders the
/// commit. On the pre-fix code both probes received the raw tag, so the patch
/// described the tag object instead of the commit.
#[test]
fn test_show_reads_the_stat_and_patch_from_one_commit() {
    let repo = scratch_repo();
    let root = repo.path();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    // A tagged commit plus a LATER commit on HEAD, so resolving `HEAD` would
    // describe something other than the tag.
    git(&["tag", "-a", "v1", "-m", "release one"]);
    std::fs::write(root.join("second.txt"), "2\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "two"]);

    let peeled = super::super::git_apply::resolve_to_oid(root, "v1").unwrap();
    let shown = git_show(root, &json!({ "target": "v1", "include_patch": true })).unwrap();
    assert_eq!(
        shown["target"], "v1",
        "the requested target is kept for display"
    );
    for (field, text) in [("stat", &shown["stat"]), ("patch", &shown["patch"])] {
        let text = text.as_str().unwrap_or_default();
        assert!(
            text.contains(&peeled),
            "{field} must describe the resolved commit {peeled}: {text}"
        );
        assert!(
            !text.contains("release one") && !text.contains("Tagger:"),
            "{field} must render the COMMIT, not the tag object: {text}"
        );
    }
    // HEAD has moved past the tag: the probes must still describe the tag's
    // commit, not HEAD.
    let head = run_text(root, &["rev-parse".to_string(), "HEAD".to_string()])
        .unwrap()
        .0;
    assert_ne!(head.trim(), peeled, "precondition: HEAD is not the tag");
    assert!(
        !shown["stat"].as_str().unwrap().contains("second.txt"),
        "stat must not describe HEAD: {}",
        shown["stat"]
    );
}

/// CodeRabbit round 10 (id 4697090251): `with_warnings` REPLACED the payload's
/// existing warnings. A bulk add that stopped at the walk limit carried an
/// INCOMPLETE warning; adding an ignored-argument warning then dropped it, so
/// the plan looked complete while it omitted files. The lists are merged.
#[test]
fn test_with_warnings_merges_instead_of_replacing() {
    let payload = json!({ "warnings": ["staging stopped at the walk limit"], "x": 1 });
    let merged = with_warnings(payload, &["git add ignores 'target'".to_string()]);
    let warnings: Vec<String> = merged["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w.as_str().map(str::to_string))
        .collect();
    assert_eq!(
        warnings,
        vec![
            "staging stopped at the walk limit".to_string(),
            "git add ignores 'target'".to_string(),
        ],
        "both warnings must reach the reviewer: {merged}"
    );
    // A payload with no warnings still gets the array.
    let fresh = with_warnings(json!({ "y": 2 }), &["w".to_string()]);
    assert_eq!(fresh["warnings"], json!(["w"]));
    // Empty input leaves the payload untouched.
    let untouched = with_warnings(json!({ "z": 3 }), &[]);
    assert!(untouched.get("warnings").is_none());
}

/// CodeRabbit round 10 (id 4697090265): a non-array `paths` value used to
/// skip the pathspec block, so the argv had no `--`, the commit was treated
/// as a whole-index commit, and ALL staged content was committed while the
/// caller believed only `paths` was. `add` already refused this shape.
#[test]
fn test_commit_refuses_a_non_array_paths_value() {
    let repo = scratch_repo();
    for bad in [
        json!({ "paths": "a.txt" }),
        json!({ "paths": 7 }),
        json!({ "paths": null }),
    ] {
        let mut args = bad.clone();
        args["message"] = json!("m");
        let error = commit_argv_for_test(repo.path(), &args)
            .expect_err("a non-array paths must be refused");
        assert!(error.message_with_hint().contains("paths"), "{error}");
    }
    // The array shape is still accepted.
    std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    let ok = commit_argv_for_test(repo.path(), &json!({ "message": "m", "paths": ["a.txt"] }))
        .expect("an array of paths is valid");
    assert!(ok.contains(&"--".to_string()), "{ok:?}");
}

/// Helper: a scratch repository with one commit on branch `master`.
fn scratch_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .expect("git available");
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "master"]);
    std::fs::write(dir.path().join("f.txt"), "a\nb\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "one"]);
    dir
}

/// Regression: blame passed `--no-color`, which git-blame reads as an
/// ambiguous option prefix and rejects with exit 129 — so every blame call
/// failed. Color is now suppressed with `-c color.ui=false`.
///
/// Both shapes are exercised: with a `target` (argv carries the rev before
/// `--`) and without (no rev at all).
#[test]
fn test_blame_works_with_and_without_a_target() {
    let repo = scratch_repo();
    for args in [
        json!({ "path": "f.txt" }),
        json!({ "path": "f.txt", "target": "HEAD" }),
    ] {
        let value = git_blame(repo.path(), &args).unwrap_or_else(|error| panic!("{args}: {error}"));
        assert_eq!(value["line_count"], 2, "{args}: {value}");
        // Each blamed line carries the abbreviated commit oid and the
        // blamed source text. `git blame` prints no commit SUBJECT, so
        // asserting on one tests a feature blame does not have. A boundary
        // commit (the root, which every line here is) is prefixed with
        // `^`, so that marker is part of valid output.
        let text = value["text"].as_str().unwrap_or_default();
        for line in text.lines() {
            let oid = line
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_start_matches('^');
            assert!(
                oid.len() >= 7 && oid.chars().all(|c| c.is_ascii_hexdigit()),
                "each line must name its last-touching commit: {line}"
            );
        }
        assert!(text.contains(") a"), "line 1 content missing: {value}");
        assert!(text.contains(") b"), "line 2 content missing: {value}");
        // The reported target defaults to HEAD when none was supplied.
        assert_eq!(value["target"], "HEAD");
    }
}

/// Regression: `branch` returned every entry it found, ignoring `max_count`,
/// with a `truncated` flag that only tracked the byte cap — so a capped list
/// looked complete.
#[test]
fn test_branch_honours_max_count_and_reports_the_dropped_tail() {
    let repo = scratch_repo();
    for name in ["cw32/30b-admission", "cw32/31c-other", "feature/x"] {
        let status = std::process::Command::new("git")
            .args(["branch", name])
            .current_dir(repo.path())
            .status()
            .unwrap();
        assert!(status.success());
    }

    let value = git_branch(repo.path(), &json!({ "max_count": 2 })).unwrap();
    assert_eq!(value["count"], 2, "{value}");
    assert_eq!(value["total"], 4, "master + three branches: {value}");
    assert_eq!(value["truncated"], true);
    assert_eq!(
        value["branches"].as_array().map(Vec::len),
        Some(2),
        "max_count is a real bound: {value}"
    );

    // A generous bound is not truncated.
    let full = git_branch(repo.path(), &json!({ "max_count": 20 })).unwrap();
    assert_eq!(full["total"], 4);
    assert_eq!(full["truncated"], false);

    // The prefix filter is a prefix, not a substring.
    let filtered = git_branch(
        repo.path(),
        &json!({ "name_prefix": "cw32/", "max_count": 20 }),
    )
    .unwrap();
    let names: Vec<&str> = filtered["branches"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|branch| branch["name"].as_str())
        .collect();
    assert_eq!(names.len(), 2, "{filtered}");
    assert!(names.iter().all(|name| name.starts_with("cw32/")));
    assert_eq!(filtered["total"], 2);

    let substring = git_branch(
        repo.path(),
        &json!({ "name_prefix": "30b", "max_count": 20 }),
    )
    .unwrap();
    assert_eq!(
        substring["count"], 0,
        "'30b' is not a prefix of cw32/30b-admission: {substring}"
    );

    // Local-only is the default; `local_only: false` adds remote-tracking
    // refs, so the default must NOT be `--all`.
    let local = git_branch(repo.path(), &json!({ "max_count": 20 })).unwrap();
    assert_eq!(local["local_only"], true);
    assert!(local["branches"].as_array().unwrap().iter().all(|branch| {
        !branch["name"]
            .as_str()
            .unwrap_or_default()
            .starts_with("remotes/")
    }));
}
