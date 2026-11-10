//! Generated checks through the real CLI, synthetic transcripts and real Git objects.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

const A: &str = "36a3723a82d823d5f6b3816f8d5e169e2a263def";
const B: &str = "36a3723b4273a295d58f417fce01d3662c4660aa";

fn runner() -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases: 32,
            max_shrink_iters: 128,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[61; 32]),
    )
}

fn git(repo: &Path, args: &[&str], input: Option<&str>) -> String {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", repo.join("no-config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

struct Fixture {
    home: tempfile::TempDir,
}
impl Fixture {
    fn new(collision: bool) -> Self {
        let f = Self {
            home: tempfile::tempdir().unwrap(),
        };
        fs::create_dir(f.home.path().join("repo")).unwrap();
        git(
            &f.home.path().join("repo"),
            &["init", "--object-format=sha1", "--initial-branch=main"],
            None,
        );
        let tree = git(
            &f.home.path().join("repo"),
            &["hash-object", "-w", "-t", "tree", "--stdin"],
            Some(""),
        );
        assert_eq!(tree, "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
        // Independently discovered SHA-1 collision at seven digits; Git verifies each object.
        for (nonce, expected) in
            [(2041, A), (26700, B)]
                .into_iter()
                .take(if collision { 2 } else { 1 })
        {
            let body = format!(
                "tree {tree}\nauthor Synthetic <synthetic@example.invalid> 1 +0000\ncommitter Synthetic <synthetic@example.invalid> 1 +0000\n\ncollision-{nonce}\n"
            );
            assert_eq!(
                git(
                    &f.home.path().join("repo"),
                    &["hash-object", "-w", "-t", "commit", "--stdin"],
                    Some(&body)
                ),
                expected
            );
        }
        f
    }
    fn write(&self, texts: &[String]) {
        let dir = self.home.path().join(".claude/projects/synthetic");
        if dir.exists() {
            fs::remove_dir_all(&dir).unwrap();
        }
        fs::create_dir_all(&dir).unwrap();
        for (i, text) in texts.iter().enumerate() {
            let record = json!({"type":"assistant", "timestamp":"2026-10-20T01:00:00Z",
                "cwd":self.home.path().join("repo"), "message":{"content":[{"type":"text","text":text}]}});
            fs::write(dir.join(format!("case-{i}.jsonl")), format!("{record}\n")).unwrap();
        }
    }
    fn run(&self, rev: &str) -> Value {
        let cache = self.home.path().join("cache");
        if cache.exists() {
            fs::remove_dir_all(&cache).unwrap();
        }
        let out = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
            .args(["commit", rev, "--json", "--harness", "claude", "--repo"])
            .arg(self.home.path().join("repo"))
            .env_clear()
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("AGENT_TRACES_NOW", "2026-10-20T01:02:03Z")
            .env("AGENT_TRACES_CACHE", cache)
            .env("AGENT_TRACES_OUT", self.home.path().join("out"))
            .env("AGENT_TRACES_HOST", "fixture-host")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.path().join("no-config"))
            .current_dir(self.home.path())
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
    }
}

fn candidate(doc: &Value, i: usize) -> Option<&Value> {
    doc["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["session"]["id"] == format!("case-{i}"))
}

#[test]
#[ignore = "formal verification property; run .claude/skills/verify-commit-evidence/scripts/check.sh"]
fn generated_token_lengths_and_prefixes() {
    let f = Fixture::new(false);
    runner()
        .run(
            &(
                any::<bool>(),
                0usize..40,
                prop::sample::select(vec![" ", "_", "\n", "🦀", "/", ":"]),
            ),
            |(upper, wrong_at, sep)| {
                let mut tokens: Vec<String> = (0..=45)
                    .map(|n| {
                        if n <= 40 {
                            A[..n].to_owned()
                        } else {
                            format!("{A}{}", "a".repeat(n - 40))
                        }
                    })
                    .collect();
                let mut wrong = A.to_owned();
                wrong.replace_range(
                    wrong_at..wrong_at + 1,
                    if &A[wrong_at..wrong_at + 1] == "0" {
                        "1"
                    } else {
                        "0"
                    },
                );
                tokens.push(wrong);
                if upper {
                    tokens = tokens.into_iter().map(|s| s.to_ascii_uppercase()).collect();
                }
                f.write(
                    &tokens
                        .iter()
                        .map(|s| format!("noise g{}g context{sep}{s}{sep}end", &A[..7]))
                        .collect::<Vec<_>>(),
                );
                let doc = f.run(A);
                prop_assert_eq!(&doc["commit"]["oid"], A);
                for (i, token) in tokens.iter().enumerate() {
                    let expected = (7..=40).contains(&i);
                    prop_assert_eq!(
                        candidate(&doc, i).is_some(),
                        expected,
                        "length/index={}, token={}",
                        i,
                        token
                    );
                    if let Some(c) = candidate(&doc, i) {
                        let claim = &c["claims"][0];
                        let lower = token.to_ascii_lowercase();
                        prop_assert_eq!(claim["recorded_sha"].as_str(), Some(lower.as_str()));
                        prop_assert_eq!(
                            &claim["sha_match"],
                            if i == 40 { "full" } else { "prefix" }
                        );
                        prop_assert_eq!(&c["best_claim"], "mentioned");
                    }
                }
                Ok(())
            },
        )
        .unwrap();
}

#[test]
#[ignore = "formal verification property; run .claude/skills/verify-commit-evidence/scripts/check.sh"]
fn generated_identifier_boundaries() {
    let f = Fixture::new(false);
    runner()
        .run(
            &(7usize..=40, "[g-zG-Z]{1,12}", any::<bool>()),
            |(n, word, upper)| {
                let token = if upper {
                    A[..n].to_ascii_uppercase()
                } else {
                    A[..n].to_owned()
                };
                let texts = vec![
                    format!("{word}{token}"),
                    format!("{token}{word}"),
                    format!("é{token}"),
                    format!("{token}中"),
                    format!("9{token}9"),
                    format!("_{token}_"),
                ];
                f.write(&texts);
                let doc = f.run(A);
                for (i, text) in texts.iter().enumerate().take(5) {
                    prop_assert!(
                        candidate(&doc, i).is_none(),
                        "identifier {:?} matched",
                        text
                    );
                }
                prop_assert!(candidate(&doc, 5).is_some(), "underscore delimiter lost");
                Ok(())
            },
        )
        .unwrap();
}

#[test]
#[ignore = "formal verification property; run .claude/skills/verify-commit-evidence/scripts/check.sh"]
fn generated_ambiguous_prefix_never_resolves() {
    let f = Fixture::new(true);
    runner()
        .run(
            &(any::<bool>(), 8usize..=40, 1usize..5),
            |(upper, n, copies)| {
                let prefix = if upper {
                    A[..7].to_ascii_uppercase()
                } else {
                    A[..7].to_owned()
                };
                f.write(&[std::iter::repeat_n(prefix.clone(), copies)
                    .collect::<Vec<_>>()
                    .join(" ")]);
                let doc = f.run(&prefix);
                prop_assert_eq!(&doc["commit"]["status"], "ambiguous");
                prop_assert!(doc["commit"]["oid"].is_null());
                prop_assert_eq!(&doc["commit"]["candidates"], &json!([A, B]));
                // Text remains a lead; ambiguity is not an instruction to erase evidence.
                let c = candidate(&doc, 0).unwrap();
                prop_assert_eq!(&c["best_claim"], "mentioned");
                prop_assert_eq!(&c["claims"][0]["abbrev_unique"], false);
                let unique = f.run(&A[..n]);
                prop_assert_eq!(&unique["commit"]["status"], "resolved");
                prop_assert_eq!(&unique["commit"]["oid"], A);
                Ok(())
            },
        )
        .unwrap();
}
