//! Rules a project judges with programs of its own (`[[rules.external]]`).
//!
//! The protocol, version [`PROTOCOL`]: nodex writes one JSON request to the
//! program's stdin and reads one JSON answer from its stdout.
//!
//! ```json
//! {"protocol": 1, "rule": "external/<name>", "tree": "/abs/dir",
//!  "documents": [{"id": "…", "path": "docs/a.md", "kind": "…", "status": "…",
//!                 "title": "…", "content": "…"}]}
//! ```
//!
//! `documents` are the graphed documents of the block's `kinds`, and
//! `content` is the text the pass judges — a proposal's bytes under
//! `check --content` or a write gate, the index's under `check --staged` —
//! so the program reads documents from the request, never from disk. `tree`
//! is the directory the documents a proposal leaves alone are read from:
//! the index's checkout under `check --staged`, and otherwise the project
//! as it stands, which does not hold what a proposal changes.
//!
//! ```json
//! {"protocol": 1, "unjudged": 0,
//!  "findings": [{"id": "…", "message": "…", "line": 3, "data": {}}]}
//! ```
//!
//! A finding names a document it was given by `id`, or none for a finding
//! about the project. `data` is what the finding is: the gates pair a
//! proposal's findings with the standing ones by rule, document and `data`,
//! so `message` and `line` can move with the text without a write reading
//! them as introduced. `unjudged` counts the documents the program could
//! not judge; the rest are the rule's `subjects`.
//!
//! The program runs in the project's directory in the operator's working
//! tree, so a staged check judges the index's bytes with the tools the
//! operator has installed. A program that fails to start, exits other than
//! zero, or answers outside the protocol refuses the command
//! ([`Error::ExternalRule`]): a rule that could not judge never passes.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::detail::{Evidence, ViolationDetails};
use super::{Rule, RuleContext, RuleRun, RuleSource, Severity, SubjectUnit, Violation};
use crate::builder::scanner::ProjectFiles;
use crate::config::{Config, ExternalRuleConfig};
use crate::diff::Touched;
use crate::error::{Error, Result};
use crate::model::{Graph, Node};

/// The protocol version a request carries and an answer must repeat.
pub const PROTOCOL: u32 = 1;

/// Characters of a failed program's stderr a refusal quotes, from its end.
const STDERR_TAIL: usize = 2000;

/// One `[[rules.external]]` block. It reads what its program answered from
/// [`RuleContext::external`], asked before the pass ran.
pub struct ExternalRule {
    config: ExternalRuleConfig,
    qualified_id: String,
}

impl ExternalRule {
    pub fn new(config: ExternalRuleConfig) -> Self {
        let qualified_id = qualified_id(&config.name);
        Self {
            config,
            qualified_id,
        }
    }
}

fn qualified_id(name: &str) -> String {
    format!("external/{name}")
}

impl Rule for ExternalRule {
    fn id(&self) -> &str {
        &self.qualified_id
    }

    fn severity(&self) -> Severity {
        self.config.severity
    }

    fn declares_severity(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "External rule: the declared program judges the documents of its kinds"
    }

    fn source(&self) -> RuleSource {
        RuleSource::Config
    }

    fn params(&self, _config: &Config) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("command".into(), json!(self.config.command));
        m.insert("kinds".into(), json!(self.config.kinds));
        m.insert("protocol".into(), json!(PROTOCOL));
        m
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        ctx.external.answer_for(&self.qualified_id)
    }

    /// Every finding: nodex cannot tell which documents, or which other
    /// files, decided what a program found, so a range that touched none of
    /// them is never known to have left a finding alone.
    fn touched_by(&self, _ctx: &RuleContext<'_>, _since: &Touched, _violation: &Violation) -> bool {
        true
    }
}

/// What each external rule's program answered for one pass. Asked before
/// any rule runs, because it is the one reading of a pass that can fail,
/// and a failure has to refuse the command rather than reach a verdict.
pub struct ExternalAnswers(BTreeMap<String, Answered>);

struct Answered {
    subjects: usize,
    unjudged: usize,
    violations: Vec<Violation>,
}

impl ExternalAnswers {
    /// A pass that runs no external rule.
    pub const fn none() -> Self {
        Self(BTreeMap::new())
    }

    /// Ask the program of every external rule among `rules` about the
    /// documents of its kinds `graph` holds, as `files` reads them. A rule
    /// whose kinds select no document guards nothing, and its program is
    /// not run.
    pub(crate) fn ask(
        rules: &[Box<dyn Rule>],
        graph: &Graph,
        config: &Config,
        files: ProjectFiles<'_>,
    ) -> Result<Self> {
        let mut answers = BTreeMap::new();
        for block in &config.rules.external {
            let id = qualified_id(&block.name);
            if !rules.iter().any(|rule| rule.id() == id) {
                continue;
            }
            let documents: Vec<&Node> = graph
                .nodes()
                .values()
                .filter(|node| node.matches_kinds(&block.kinds))
                .collect();
            let answered = if documents.is_empty() {
                Answered {
                    subjects: 0,
                    unjudged: 0,
                    violations: Vec::new(),
                }
            } else {
                let texts = files.graphed_texts(&documents, config.scope.follow_symlinks)?;
                ask(block, &id, &documents, texts, files)?
            };
            answers.insert(id, answered);
        }
        Ok(Self(answers))
    }

    fn answer_for(&self, id: &str) -> RuleRun {
        let answered = self
            .0
            .get(id)
            .expect("a pass asks every external rule it runs before running it");
        RuleRun {
            subjects: answered.subjects,
            unjudged: answered.unjudged,
            violations: answered.violations.clone(),
        }
    }
}

#[derive(Serialize)]
struct Request<'a> {
    protocol: u32,
    rule: &'a str,
    tree: String,
    documents: Vec<Document<'a>>,
}

#[derive(Serialize)]
struct Document<'a> {
    id: &'a str,
    path: String,
    kind: &'a str,
    status: &'a str,
    title: &'a str,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    protocol: u32,
    #[serde(default)]
    unjudged: usize,
    #[serde(default)]
    findings: Vec<Finding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finding {
    #[serde(default)]
    id: Option<String>,
    message: String,
    #[serde(default)]
    line: Option<usize>,
    #[serde(default)]
    data: Map<String, Value>,
}

fn ask(
    block: &ExternalRuleConfig,
    id: &str,
    documents: &[&Node],
    texts: Vec<String>,
    files: ProjectFiles<'_>,
) -> Result<Answered> {
    let sent = documents
        .iter()
        .zip(texts)
        .map(|(node, content)| Document {
            id: &node.id,
            path: crate::path_guard::forward_string(&node.path),
            kind: node.kind.as_str(),
            status: node.status.as_str(),
            title: &node.title,
            content,
        })
        .collect();
    let tree = std::path::absolute(files.root()).map_err(|source| Error::Io {
        path: files.root().to_path_buf(),
        source,
    })?;
    let request = serde_json::to_vec(&Request {
        protocol: PROTOCOL,
        rule: id,
        tree: tree.to_string_lossy().into_owned(),
        documents: sent,
    })
    .map_err(|e| Error::Internal(format!("external rule request did not encode: {e}")))?;
    let failed = |reason: String| Error::ExternalRule {
        rule: id.to_string(),
        reason,
    };
    let output = run(&block.command, files.work_tree(), &request).map_err(failed)?;
    let refused = |reason: String| failed(format!("{reason}{}", stderr_note(&output.stderr)));
    let answer: Answer = serde_json::from_slice(&output.stdout)
        .map_err(|e| refused(format!("its answer is not protocol {PROTOCOL}: {e}")))?;
    judge(answer, id, block.severity, documents).map_err(refused)
}

/// Run `command` in `work_tree` with `request` on its stdin, returning what
/// it wrote once it exits cleanly.
fn run(
    command: &[String],
    work_tree: &Path,
    request: &[u8],
) -> std::result::Result<std::process::Output, String> {
    let (program, args) = command
        .split_first()
        .expect("load refuses an external rule without a program");
    let path = Path::new(program);
    let resolved = if path.components().count() > 1 {
        work_tree.join(path)
    } else {
        path.to_path_buf()
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "an external rule runs the program its block declares, which is never git"
    )]
    let mut child = std::process::Command::new(resolved)
        .args(args)
        .current_dir(work_tree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program:?} did not start: {e}"))?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    // Written beside the wait: a program that answers before it has read the
    // whole request would otherwise fill its stdout pipe while nodex is still
    // blocked filling its stdin.
    let (written, output) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(request));
        let output = child.wait_with_output();
        (
            writer.join().expect("the stdin writer does not panic"),
            output,
        )
    });
    let output = output.map_err(|e| format!("{program:?} could not be waited on: {e}"))?;
    if !output.status.success() {
        let how = match output.status.code() {
            Some(code) => format!("exited with status {code}"),
            None => "was stopped by a signal".to_string(),
        };
        return Err(format!("{program:?} {how}{}", stderr_note(&output.stderr)));
    }
    // A program may answer without reading every document it was handed.
    if let Err(e) = written
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(format!(
            "{program:?} could not be handed the documents: {e}"
        ));
    }
    Ok(output)
}

fn stderr_note(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let start = text
        .char_indices()
        .rev()
        .nth(STDERR_TAIL - 1)
        .map_or(0, |(at, _)| at);
    format!("; stderr: {}", &text[start..])
}

/// Read `answer` as findings on `documents`, refusing anything the protocol
/// does not let a program say.
fn judge(
    answer: Answer,
    id: &str,
    severity: Severity,
    documents: &[&Node],
) -> std::result::Result<Answered, String> {
    if answer.protocol != PROTOCOL {
        return Err(format!(
            "it answered protocol {}, and this nodex speaks protocol {PROTOCOL}",
            answer.protocol
        ));
    }
    if answer.unjudged > documents.len() {
        return Err(format!(
            "it counted {} documents unjudged of the {} it was given",
            answer.unjudged,
            documents.len()
        ));
    }
    let subjects = documents.len() - answer.unjudged;
    if subjects == 0 && !answer.findings.is_empty() {
        return Err(
            "it reported findings while counting every document it was given unjudged".to_string(),
        );
    }
    let mut violations = Vec::with_capacity(answer.findings.len());
    for finding in answer.findings {
        if finding.message.trim().is_empty() {
            return Err("it reported a finding with no message".to_string());
        }
        let node = match &finding.id {
            Some(subject) => Some(
                documents
                    .iter()
                    .find(|node| node.id == *subject)
                    .ok_or_else(|| {
                        format!(
                            "it reported a finding on {subject:?}, which is not among the \
                             documents it was given"
                        )
                    })?,
            ),
            None => None,
        };
        match (finding.line, node) {
            (Some(0), _) => {
                return Err("it reported a finding at line 0; lines are one-based".to_string());
            }
            (Some(_), None) => {
                return Err("it reported a line for a finding that names no document".to_string());
            }
            _ => {}
        }
        violations.push(Violation::new(
            id,
            severity,
            node.map(|node| node.id.clone()),
            node.map(|node| crate::path_guard::forward_string(&node.path)),
            ViolationDetails::External {
                message: Evidence(finding.message),
                line: Evidence(finding.line),
                data: finding.data,
            },
        ));
    }
    Ok(Answered {
        subjects,
        unjudged: answer.unjudged,
        violations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str) -> Node {
        Node {
            id: id.into(),
            path: format!("docs/{id}.md").into(),
            title: id.into(),
            kind: crate::model::Kind::new("generic"),
            status: crate::model::Status::new("active"),
            created: None,
            updated: None,
            reviewed: None,
            owner: None,
            supersedes: vec![],
            superseded_by: None,
            implements: vec![],
            related: vec![],
            tags: vec![],
            covers: vec![],
            orphan_ok: false,
            attrs: BTreeMap::new(),
            frontmatter_relations: Default::default(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
            body_structure: Default::default(),
            body_start_line: 1,
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        }
    }

    fn answer(json: &str) -> Answer {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_finding_is_identified_by_its_document_and_data_not_its_wording() {
        let a = node("a");
        let documents = [&a];
        let read = |json: &str| {
            judge(answer(json), "external/x", Severity::Error, &documents)
                .unwrap()
                .violations
        };
        let before = read(
            r#"{"protocol": 1, "findings": [
                {"id": "a", "message": "word \"foo\" at line 3", "line": 3, "data": {"word": "foo"}}]}"#,
        );
        let moved = read(
            r#"{"protocol": 1, "findings": [
                {"id": "a", "message": "word \"foo\" at line 4", "line": 4, "data": {"word": "foo"}}]}"#,
        );
        let other = read(
            r#"{"protocol": 1, "findings": [
                {"id": "a", "message": "word \"bar\"", "data": {"word": "bar"}}]}"#,
        );
        assert_eq!(before[0].path.as_deref(), Some("docs/a.md"));
        assert_eq!(before[0].message, "line 3: word \"foo\" at line 3");
        assert!(super::super::introduced_violations(moved, &before).is_empty());
        assert_eq!(super::super::introduced_violations(other, &before).len(), 1);
    }

    #[test]
    fn the_subjects_are_the_documents_the_program_judged() {
        let (a, b) = (node("a"), node("b"));
        let answered = judge(
            answer(r#"{"protocol": 1, "unjudged": 1}"#),
            "external/x",
            Severity::Error,
            &[&a, &b],
        )
        .unwrap();
        assert_eq!((answered.subjects, answered.unjudged), (1, 1));
    }

    #[test]
    fn an_answer_outside_the_protocol_is_refused_naming_what_it_said() {
        let a = node("a");
        let documents = [&a];
        for (json, refusal) in [
            (r#"{"protocol": 2}"#, "protocol 2"),
            (
                r#"{"protocol": 1, "unjudged": 2}"#,
                "2 documents unjudged of the 1",
            ),
            (
                r#"{"protocol": 1, "unjudged": 1, "findings": [{"message": "m"}]}"#,
                "counting every document it was given unjudged",
            ),
            (
                r#"{"protocol": 1, "findings": [{"id": "b", "message": "m"}]}"#,
                "\"b\", which is not among",
            ),
            (
                r#"{"protocol": 1, "findings": [{"message": " "}]}"#,
                "no message",
            ),
            (
                r#"{"protocol": 1, "findings": [{"id": "a", "message": "m", "line": 0}]}"#,
                "line 0",
            ),
            (
                r#"{"protocol": 1, "findings": [{"message": "m", "line": 2}]}"#,
                "names no document",
            ),
        ] {
            let Err(reason) = judge(answer(json), "external/x", Severity::Error, &documents) else {
                panic!("{json} was accepted");
            };
            assert!(reason.contains(refusal), "{json}: {reason}");
        }
        assert!(
            serde_json::from_str::<Answer>(r#"{"protocol": 1, "finding": []}"#).is_err(),
            "a misspelt key is refused rather than read as no findings"
        );
    }
}
