use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::ancestry::Step;
use crate::config::{BodyImmutableMode, Config, ImmutableHistory};
use crate::diff::GraphDiff;
use crate::model::{Edge, Graph, Node, ReferenceLocation, ReferenceProtection, ReferenceSource};

use super::DocumentPart;

#[derive(Clone, Copy)]
enum Limit {
    All,
    /// The committed prefix of an `append_only` body, as the last file line
    /// it covers.
    Prefix(usize),
    Unknown,
}

/// Shares baseline and step readings across every citation of each document part.
pub(crate) struct ReferenceLocks<'a> {
    graph: &'a Graph,
    config: &'a Config,
    diff: Option<&'a GraphDiff>,
    added: BTreeSet<&'a str>,
    step: Option<&'a Step>,
    replaced: BTreeMap<&'a Path, Vec<&'a str>>,
    limits: BTreeMap<(String, DocumentPart), Vec<(String, Limit)>>,
}

impl<'a> ReferenceLocks<'a> {
    pub(crate) fn new(
        graph: &'a Graph,
        config: &'a Config,
        diff: Option<&'a GraphDiff>,
        steps: Option<&'a [Step]>,
    ) -> Self {
        let step = (config.rules.immutable_history == ImmutableHistory::Steps
            && diff.is_some()
            && (!config.rules.body_immutable.is_empty()
                || !config.rules.frontmatter_immutable.is_empty()))
        .then(|| steps.and_then(|steps| steps.iter().find(|step| step.commit.is_none())))
        .flatten();
        Self {
            graph,
            config,
            diff,
            added: diff.map(GraphDiff::added_ids).unwrap_or_default(),
            step,
            replaced: step.map(super::replaced_records).unwrap_or_default(),
            limits: BTreeMap::new(),
        }
    }

    pub(crate) fn sources(&mut self, edge: &Edge) -> Vec<ReferenceSource> {
        edge.references
            .iter()
            .map(|reference| {
                let part = match &reference.location {
                    ReferenceLocation::Body { .. } => DocumentPart::Body,
                    ReferenceLocation::Frontmatter { field } => DocumentPart::Field(field.clone()),
                };
                let key = (reference.source.clone(), part.clone());
                if !self.limits.contains_key(&key) {
                    let limits = self.measure(&reference.source, &part);
                    self.limits.insert(key.clone(), limits);
                }
                let limits = &self.limits[&key];
                let mut protection = if self.graph.node(&reference.source).is_some() {
                    ReferenceProtection::Unprotected
                } else {
                    ReferenceProtection::Unknown
                };
                let mut rules = BTreeSet::new();
                for (rule, limit) in limits {
                    let state = match (limit, &reference.location) {
                        (Limit::All, _) => ReferenceProtection::Protected,
                        (Limit::Prefix(lines), ReferenceLocation::Body { end_line, .. })
                            if end_line <= lines =>
                        {
                            ReferenceProtection::Protected
                        }
                        (Limit::Prefix(_), _) => ReferenceProtection::Unprotected,
                        (Limit::Unknown, _) => ReferenceProtection::Unknown,
                    };
                    if state != ReferenceProtection::Unprotected {
                        rules.insert(rule.clone());
                    }
                    protection = combine_constraints(protection, state);
                }
                ReferenceSource {
                    reference: reference.clone(),
                    protection,
                    rules: rules.into_iter().collect(),
                }
            })
            .collect()
    }

    fn measure(&self, id: &str, part: &DocumentPart) -> Vec<(String, Limit)> {
        let Some(node) = self.graph.node(id) else {
            return Vec::new();
        };
        let mut limits = Vec::new();
        match part {
            DocumentPart::Body => {
                for block in &self.config.rules.body_immutable {
                    let rule = format!("body_immutable/{}", block.name);
                    let same = |before: &Node, after: &Node| before.body_hash == after.body_hash;
                    if self.baseline_arms(node, block.arming()) {
                        let limit = self.diff.map_or(Limit::Unknown, |diff| {
                            diff.body_changes
                                .iter()
                                .find(|change| change.id == id)
                                .map_or(Limit::All, |change| {
                                    body_limit(
                                        block.mode,
                                        &change.before_hash,
                                        &change.before_lines_hash,
                                        node,
                                    )
                                })
                        });
                        limits.push((rule.clone(), limit));
                    }
                    self.step_limits(
                        node,
                        block.arming(),
                        &same,
                        |before| {
                            body_limit(block.mode, &before.body_hash, &before.body_lines_hash, node)
                        },
                        &rule,
                        &mut limits,
                    );
                }
            }
            DocumentPart::Field(field) => {
                for block in &self.config.rules.frontmatter_immutable {
                    if !block.locks_field(field) {
                        continue;
                    }
                    let rule = format!("frontmatter_immutable/{}", block.name);
                    let same = |before: &Node, after: &Node| {
                        crate::diff::same_frontmatter_fields(before, after, |field| {
                            block.locks_field(field)
                        })
                    };
                    if self.baseline_arms(node, block.arming()) {
                        let limit = self.diff.map_or(Limit::Unknown, |diff| {
                            if diff
                                .field_changes
                                .iter()
                                .any(|change| change.id == id && change.field == *field)
                            {
                                Limit::Unknown
                            } else {
                                Limit::All
                            }
                        });
                        limits.push((rule.clone(), limit));
                    }
                    self.step_limits(
                        node,
                        block.arming(),
                        &same,
                        |before| {
                            if crate::diff::same_frontmatter_fields(before, node, |name| {
                                name == field
                            }) {
                                Limit::All
                            } else {
                                Limit::Unknown
                            }
                        },
                        &rule,
                        &mut limits,
                    );
                }
            }
        }
        limits
    }

    fn baseline_arms(&self, node: &Node, lock: crate::config::LockArming<'_>) -> bool {
        if self.added.contains(node.id.as_str()) {
            return false;
        }
        let kind = self.diff.map_or(node.kind.as_str(), |diff| {
            diff.before_kind(&node.id, node.kind.as_str())
        });
        let status = self.diff.map_or(node.status.as_str(), |diff| {
            diff.before_status(&node.id, node.status.as_str())
        });
        super::lock_holds(self.config, lock, kind, status)
    }

    fn step_limits(
        &self,
        node: &Node,
        lock: crate::config::LockArming<'_>,
        same: &dyn Fn(&Node, &Node) -> bool,
        limit: impl Fn(&Node) -> Limit,
        rule: &str,
        limits: &mut Vec<(String, Limit)>,
    ) {
        let Some(step) = self.step else { return };
        if step.child.document(&node.id).len() != 1 {
            return;
        }
        let (priors, known) =
            super::lock_priors(self.config, lock, step, node, &self.replaced, same);
        if !known {
            if super::lock_holds(self.config, lock, node.kind.as_str(), node.status.as_str()) {
                limits.push((rule.to_string(), Limit::Unknown));
            }
            return;
        }
        for before in priors {
            if super::lock_holds(
                self.config,
                lock,
                before.kind.as_str(),
                before.status.as_str(),
            ) {
                limits.push((rule.to_string(), limit(before)));
            }
        }
    }
}

fn body_limit(mode: BodyImmutableMode, hash: &str, lines: &[String], node: &Node) -> Limit {
    if hash == node.body_hash {
        Limit::All
    } else if mode == BodyImmutableMode::AppendOnly && node.body_lines_hash.starts_with(lines) {
        Limit::Prefix(node.body_start_line + lines.len() - 1)
    } else {
        Limit::Unknown
    }
}

fn combine_constraints(
    left: ReferenceProtection,
    right: ReferenceProtection,
) -> ReferenceProtection {
    use ReferenceProtection::*;
    match (left, right) {
        (Protected, _) | (_, Protected) => Protected,
        (Unknown, _) | (_, Unknown) => Unknown,
        _ => Unprotected,
    }
}

/// A relation is wholly protected only when every authored source is protected.
pub(crate) fn protection(sources: &[ReferenceSource]) -> ReferenceProtection {
    use ReferenceProtection::*;
    if sources.is_empty() {
        Unknown
    } else if sources
        .iter()
        .any(|source| source.protection == Unprotected)
    {
        Unprotected
    } else if sources.iter().any(|source| source.protection == Unknown) {
        Unknown
    } else {
        Protected
    }
}
