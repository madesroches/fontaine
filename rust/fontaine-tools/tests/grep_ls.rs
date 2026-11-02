mod common;

use std::path::Path;

use common::Dirs;
use fontaine_tools::builtins::{Grep, Ls, list_for, search_for};
use rig_core::tool::{FilesRead, Tool, ToolContext, ToolExecutionError};
use rig_tools::FilesListed;
use serde_json::{Value, json};

fn write(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

async fn grep_in(root: &Path, args: Value) -> Result<(String, ToolContext), ToolExecutionError> {
    let tool = Grep {
        search: search_for(root),
    };
    let mut context = ToolContext::new();
    let args = serde_json::from_value(args).expect("arguments parse");
    let text = tool.call(&mut context, args).await?;
    Ok((text, context))
}

async fn grep(root: &Path, args: Value) -> String {
    grep_in(root, args).await.unwrap().0
}

async fn ls_in(root: &Path, args: Value) -> Result<(String, ToolContext), ToolExecutionError> {
    let tool = Ls {
        list: list_for(root),
    };
    let mut context = ToolContext::new();
    let args = serde_json::from_value(args).expect("arguments parse");
    let text = tool.call(&mut context, args).await?;
    Ok((text, context))
}

async fn ls(root: &Path, args: Value) -> String {
    ls_in(root, args).await.unwrap().0
}

fn read_paths(context: &ToolContext) -> Vec<String> {
    let read = context.result::<FilesRead>().unwrap().unwrap_or_default();
    read.0.into_iter().map(|file| file.path).collect()
}

fn fixture() -> (Dirs, std::path::PathBuf) {
    let d = Dirs::new();
    let root = d.dir("work");
    write(&root, "a.rs", "one\nTwo\nthree\nfour\nfive\nsix\nseven\n");
    write(&root, "b.py", "two\nTWO\nnothing\n");
    write(&root, "sub/c.rs", "no\nmatch\n");
    (d, root)
}

#[tokio::test]
async fn default_mode_lists_paths() {
    let (_d, root) = fixture();
    assert_eq!(grep(&root, json!({"pattern": "two"})).await, "b.py");
}

#[tokio::test]
async fn case_insensitive_and_inline_flags() {
    let (_d, root) = fixture();
    let all = grep(&root, json!({"pattern": "two", "-i": true})).await;
    assert_eq!(all, "a.rs\nb.py");
    let off = grep(&root, json!({"pattern": "(?-i)two", "-i": true})).await;
    assert_eq!(off, "b.py");
}

#[tokio::test]
async fn content_mode_numbers_and_toggles() {
    let (_d, root) = fixture();
    let numbered = grep(
        &root,
        json!({"pattern": "two", "output_mode": "content", "-i": true}),
    )
    .await;
    assert_eq!(numbered, "a.rs:2: Two\nb.py:1: two\nb.py:2: TWO");
    let plain = grep(
        &root,
        json!({"pattern": "Two", "output_mode": "content", "-n": false}),
    )
    .await;
    assert_eq!(plain, "a.rs: Two");
}

#[tokio::test]
async fn context_lines_and_separators() {
    let (_d, root) = fixture();
    let out = grep(
        &root,
        json!({"pattern": "Two|six", "glob": "a.rs", "output_mode": "content", "-C": 1}),
    )
    .await;
    assert_eq!(
        out,
        "a.rs-1- one\na.rs:2: Two\na.rs-3- three\n--\na.rs-5- five\na.rs:6: six\na.rs-7- seven"
    );
    let adjacent = grep(
        &root,
        json!({"pattern": "Two|four", "glob": "a.rs", "output_mode": "content", "-C": 1}),
    )
    .await;
    assert_eq!(
        adjacent,
        "a.rs-1- one\na.rs:2: Two\na.rs-3- three\na.rs:4: four\na.rs-5- five"
    );
}

#[tokio::test]
async fn explicit_after_and_before_override_context() {
    let (_d, root) = fixture();
    let out = grep(
        &root,
        json!({"pattern": "four", "output_mode": "content", "-C": 2, "-A": 0, "-B": 1}),
    )
    .await;
    assert_eq!(out, "a.rs-3- three\na.rs:4: four");
    let start = grep(
        &root,
        json!({"pattern": "one", "output_mode": "content", "-B": 3, "-A": 1, "-n": false}),
    )
    .await;
    assert_eq!(start, "a.rs: one\na.rs- Two");
}

#[tokio::test]
async fn context_lines_without_numbers() {
    let (_d, root) = fixture();
    let out = grep(
        &root,
        json!({"pattern": "three", "output_mode": "content", "-C": 1, "-n": false}),
    )
    .await;
    assert_eq!(out, "a.rs- Two\na.rs: three\na.rs- four");
}

#[tokio::test]
async fn head_limit_cuts_matches_and_keeps_trailing_context() {
    let (_d, root) = fixture();
    let out = grep(
        &root,
        json!({"pattern": "e", "glob": "a.rs", "output_mode": "content", "head_limit": 2, "-A": 1}),
    )
    .await;
    assert_eq!(
        out,
        "a.rs:1: one\na.rs-2- Two\na.rs:3: three\na.rs-4- four\n[stopped at 2 matches]"
    );
}

#[tokio::test]
async fn count_omits_files_without_a_match() {
    let (_d, root) = fixture();
    let out = grep(&root, json!({"pattern": "o", "output_mode": "count"})).await;
    assert_eq!(out, "a.rs:3\nb.py:2\nsub/c.rs:1");
    let none = grep(&root, json!({"pattern": "zzz", "output_mode": "count"})).await;
    assert_eq!(none, "No matches.");
}

#[tokio::test]
async fn head_limit_stop_lines_per_mode() {
    let (_d, root) = fixture();
    let files = grep(&root, json!({"pattern": "o", "head_limit": 1})).await;
    assert_eq!(
        files,
        "a.rs\n[stopped at 1 files; narrow the search with path or glob]"
    );
    let count = grep(
        &root,
        json!({"pattern": "o", "output_mode": "count", "head_limit": 2}),
    )
    .await;
    assert_eq!(
        count,
        "a.rs:3\nb.py:2\n[stopped at 2 files; narrow the search with path or glob]"
    );
    let exact = grep(&root, json!({"pattern": "o", "head_limit": 3})).await;
    assert_eq!(exact, "a.rs\nb.py\nsub/c.rs");
}

#[tokio::test]
async fn head_limit_is_clamped_and_zero_is_unlimited() {
    let d = Dirs::new();
    let root = d.dir("big");
    let text: String = (0..250).map(|n| format!("hit {n}\n")).collect();
    write(&root, "big.txt", &text);
    let capped = grep(
        &root,
        json!({"pattern": "hit", "output_mode": "content", "head_limit": 1000}),
    )
    .await;
    assert_eq!(capped.lines().count(), 201);
    assert!(capped.ends_with("[stopped at 200 matches]"));
    let zero = grep(
        &root,
        json!({"pattern": "hit", "output_mode": "content", "head_limit": 0}),
    )
    .await;
    assert_eq!(zero, capped);
}

#[tokio::test]
async fn type_filters_and_combines_with_glob() {
    let (_d, root) = fixture();
    let rust = grep(&root, json!({"pattern": "o", "type": "rust"})).await;
    assert_eq!(rust, "a.rs\nsub/c.rs");
    let both = grep(
        &root,
        json!({"pattern": "o", "type": "rust", "glob": "sub/*"}),
    )
    .await;
    assert_eq!(both, "sub/c.rs");
    let disjoint = grep(&root, json!({"pattern": "o", "type": "py", "glob": "*.rs"})).await;
    assert_eq!(disjoint, "No matches.");
    let unknown = grep_in(&root, json!({"pattern": "o", "type": "nosuchtype"})).await;
    assert!(unknown.err().unwrap().to_string().contains("nosuchtype"));
}

#[tokio::test]
async fn hidden_files_need_the_flag() {
    let (_d, root) = fixture();
    write(&root, ".github/ci.yml", "two\n");
    assert_eq!(grep(&root, json!({"pattern": "two"})).await, "b.py");
    let hidden = grep(&root, json!({"pattern": "two", "hidden": true})).await;
    assert_eq!(hidden, ".github/ci.yml\nb.py");
}

#[tokio::test]
async fn unreadable_text_is_skipped() {
    let (_d, root) = fixture();
    std::fs::write(root.join("bin.dat"), [0xff, 0xfe, b't', b'w', b'o', b'\n']).unwrap();
    let big = vec![b'x'; rig_tools::MAX_FILE_BYTES as usize + 1];
    std::fs::write(root.join("big.txt"), big).unwrap();
    assert_eq!(grep(&root, json!({"pattern": "two"})).await, "b.py");
    assert_eq!(
        grep(&root, json!({"pattern": "x{10}"})).await,
        "No matches."
    );
}

#[tokio::test]
async fn errors_are_told_apart() {
    let (_d, root) = fixture();
    let bad = grep_in(&root, json!({"pattern": "("})).await;
    assert!(bad.err().unwrap().to_string().contains("bad pattern"));
    let glob = grep_in(&root, json!({"pattern": "x", "glob": "[a"})).await;
    assert!(glob.err().unwrap().to_string().contains("bad glob"));
    let missing = grep_in(&root, json!({"pattern": "x", "path": "nope"})).await;
    assert!(missing.err().unwrap().to_string().contains("nope"));
    let outside = grep_in(&root, json!({"pattern": "x", "path": ".."})).await;
    assert!(outside.err().unwrap().to_string().contains("outside"));
}

#[tokio::test]
async fn displayed_paths_are_below_the_root_in_every_mode() {
    let (_d, root) = fixture();
    for mode in ["content", "count", "files_with_matches"] {
        let out = grep(
            &root,
            json!({"pattern": "match", "path": "sub", "output_mode": mode}),
        )
        .await;
        assert!(
            out.starts_with("c.rs") || out.starts_with("sub/c.rs"),
            "{out}"
        );
    }
    let from_root = grep(&root, json!({"pattern": "match", "output_mode": "count"})).await;
    assert_eq!(from_root, "sub/c.rs:1");
    let absolute = grep(
        &root,
        json!({"pattern": "match", "path": root.join("sub").to_str().unwrap()}),
    )
    .await;
    assert_eq!(absolute, root.join("sub/c.rs").to_str().unwrap());
}

#[tokio::test]
async fn files_read_is_published_for_every_matching_file() {
    let (_d, root) = fixture();
    let expected = vec![
        root.join("a.rs").to_str().unwrap().to_owned(),
        root.join("b.py").to_str().unwrap().to_owned(),
    ];
    for args in [
        json!({"pattern": "two", "-i": true}),
        json!({"pattern": "two", "-i": true, "output_mode": "count"}),
        json!({"pattern": "two", "-i": true, "output_mode": "content"}),
        json!({"pattern": "two", "-i": true, "output_mode": "content", "head_limit": 1}),
        json!({"pattern": "two", "-i": true, "head_limit": 1}),
    ] {
        let (_, context) = grep_in(&root, args.clone()).await.unwrap();
        assert_eq!(read_paths(&context), expected, "{args}");
    }
}

#[tokio::test]
async fn an_empty_pattern_lists_files_and_reads_none() {
    let (_d, root) = fixture();
    let (listing, context) = grep_in(&root, json!({"pattern": ""})).await.unwrap();
    assert_eq!(listing, "a.rs\nb.py\nsub/c.rs");
    assert!(read_paths(&context).is_empty());
    let legacy = grep(&root, json!({"pattern": "", "files": true, "type": "py"})).await;
    assert_eq!(legacy, "b.py");
    for mode in ["content", "count"] {
        let error = grep_in(&root, json!({"pattern": "", "output_mode": mode})).await;
        assert!(error.is_err(), "{mode}");
    }
    let wins = grep_in(
        &root,
        json!({"pattern": "", "files": true, "output_mode": "count"}),
    )
    .await;
    assert!(wins.is_err());
}

#[tokio::test]
async fn output_mode_wins_over_the_files_alias() {
    let (_d, root) = fixture();
    let out = grep(
        &root,
        json!({"pattern": "match", "files": true, "output_mode": "content"}),
    )
    .await;
    assert_eq!(out, "sub/c.rs:2: match");
}

fn ls_fixture() -> (Dirs, std::path::PathBuf) {
    let d = Dirs::new();
    let root = d.dir("work");
    for name in [
        "a.rs",
        "run.log",
        "src/main.rs",
        "src/gen/out.rs",
        "node_modules/x.js",
        "pkg/node_modules/y.js",
        "pkg/lib.rs",
        ".hidden/h.rs",
    ] {
        write(&root, name, "x");
    }
    (d, root)
}

#[tokio::test]
async fn ls_ignore_matches_files_directories_and_ancestors() {
    let (_d, root) = ls_fixture();
    let out = ls(
        &root,
        json!({"ignore": ["*.log", "node_modules", "src/gen"]}),
    )
    .await;
    assert_eq!(out, "a.rs\npkg/lib.rs\nsrc/main.rs\n");
    let dir_glob = ls(&root, json!({"ignore": ["src/"]})).await;
    assert!(!dir_glob.contains("src/"), "{dir_glob}");
    assert!(dir_glob.contains("run.log"));
}

#[tokio::test]
async fn ls_ignore_combines_with_depth_glob_and_hidden() {
    let (_d, root) = ls_fixture();
    let depth = ls(&root, json!({"ignore": ["node_modules"], "depth": 1})).await;
    assert_eq!(depth, "a.rs\npkg/\nrun.log\nsrc/\n");
    let glob = ls(&root, json!({"ignore": ["src/gen"], "glob": "*.rs"})).await;
    assert_eq!(glob, "a.rs\npkg/lib.rs\nsrc/main.rs\n");
    let hidden = ls(
        &root,
        json!({"ignore": ["h.rs"], "hidden": true, "glob": "*.rs"}),
    )
    .await;
    assert!(!hidden.contains(".hidden"), "{hidden}");
    let hidden_kept = ls(&root, json!({"hidden": true, "glob": "*.rs"})).await;
    assert!(hidden_kept.contains(".hidden/h.rs"));
}

#[tokio::test]
async fn ls_ignore_errors_and_file_roots() {
    let (_d, root) = ls_fixture();
    let bad = ls_in(&root, json!({"ignore": ["["]})).await;
    assert!(bad.err().unwrap().to_string().contains("bad ignore glob"));
    let file = ls(&root, json!({"path": "a.rs", "ignore": ["a.rs"]})).await;
    assert_eq!(file, format!("{}\n", root.join("a.rs").display()));
}

#[tokio::test]
async fn ls_publishes_what_it_listed() {
    let (_d, root) = ls_fixture();
    let (_, context) = ls_in(
        &root,
        json!({"ignore": ["node_modules", "*.log"], "depth": 1}),
    )
    .await
    .unwrap();
    let listed = context.result::<FilesListed>().unwrap().unwrap();
    assert_eq!(listed.entries, ["a.rs", "pkg/", "src/"]);
}
