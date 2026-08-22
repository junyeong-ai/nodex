//! Merged per-kind runtime views (`types_for`, `enums_for`, `required_for`,
//! …) and the initial-status resolver — the read API `check`, `scaffold`,
//! and the parser consume. Every accessor returns the same merged view a
//! validator checked at load, so runtime and load-time never disagree.

use std::collections::BTreeMap;

use super::predicate::parse_when;
use super::types::*;

impl Config {
    /// Merged view: return every field-type constraint that applies to
    /// a given kind (global + first matching override). Scaffold and
    /// rules use this so every declared constraint is honoured once.
    pub fn types_for(&self, kind: &str) -> BTreeMap<String, FieldType> {
        let mut out = self.schema.types.clone();
        if let Some(ov) = self.schema_override_for(kind) {
            for (k, v) in &ov.types {
                out.insert(k.clone(), *v);
            }
        }
        out
    }

    /// Merged view: every enum constraint a project *explicitly declares*
    /// for a given kind (global `[schema]` + first matching override).
    /// Does not include the implicit `kind` / `status` vocabularies — use
    /// [`Config::effective_enums_for`] for the view the runtime enforces.
    pub fn enums_for(&self, kind: &str) -> BTreeMap<String, Vec<String>> {
        let mut out = self.schema.enums.clone();
        if let Some(ov) = self.schema_override_for(kind) {
            for (k, v) in &ov.enums {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }

    /// The *effective* enum view the runtime enforces: the explicitly
    /// declared enums ([`Config::enums_for`]) plus the implicit `kind` /
    /// `status` vocabularies backfilled from `kinds.allowed` /
    /// `statuses.allowed` when no per-kind override narrows them.
    /// Declaring `kinds.allowed` means "these and only these kinds",
    /// even without an explicit `schema.enums.kind`.
    ///
    /// The single seam for that backfill: `FieldEnumRule` (runtime check),
    /// `validate_merged_cross_fields` (load-time predicate-value check),
    /// and [`Config::allowed_statuses_for`] all read it, so the value a
    /// field may hold is identical at load time and check time — the
    /// "load mirrors runtime" invariant is enforced by construction, not
    /// by three copies of the same backfill.
    pub fn effective_enums_for(&self, kind: &str) -> BTreeMap<String, Vec<String>> {
        let mut out = self.enums_for(kind);
        out.entry("kind".to_string())
            .or_insert_with(|| self.kinds.allowed.clone());
        out.entry("status".to_string())
            .or_insert_with(|| self.statuses.allowed.clone());
        out
    }

    /// The statuses a document of `kind` may carry: the narrowing
    /// `status` enum (global `[schema]` or a `[[schema.overrides]]`
    /// block) when one is declared, else the full `statuses.allowed`
    /// set. Derived from [`Config::effective_enums_for`] so `check`'s
    /// field-enum rule and a `lifecycle` write consult one source — a
    /// transition is refused at the write seam exactly when its target
    /// status would fail the same project's `check`.
    pub fn allowed_statuses_for(&self, kind: &str) -> Vec<String> {
        self.effective_enums_for(kind)
            .remove("status")
            .expect("effective_enums_for always backfills status")
    }

    /// Merged view: every cross-field constraint that applies to a
    /// given kind. Global and override entries accumulate; an override
    /// never silently drops a global rule.
    pub fn cross_field_for(&self, kind: &str) -> Vec<CrossFieldSpec> {
        let mut out = self.schema.cross_field.clone();
        if let Some(ov) = self.schema_override_for(kind) {
            out.extend_from_slice(&ov.cross_field);
        }
        out
    }

    /// Check whether a status string is terminal.
    pub fn is_terminal(&self, status: &str) -> bool {
        self.statuses.terminal.iter().any(|t| t == status)
    }

    /// Whether nodes of the given kind are exempt from orphan detection.
    ///
    /// Driven by `detection.orphan_ok_kinds`. Pairs with the per-instance
    /// `node.orphan_ok` opt-out so callers can express both "this entire
    /// kind is leaf-by-design" and "this specific document is exceptional".
    /// Named to mirror the field and the per-node flag, paralleling
    /// `is_terminal` ↔ `statuses.terminal`.
    pub fn is_orphan_ok_kind(&self, kind: &str) -> bool {
        self.detection.orphan_ok_kinds.iter().any(|k| k == kind)
    }

    /// Merged view: every required field that applies to a given kind —
    /// the global `schema.required` unioned with the first matching
    /// override's `required` (deduplicated, globals first). An override
    /// *adds* per-kind required fields and never silently drops a global
    /// one, symmetric with `types_for` / `enums_for` / `cross_field_for`
    /// and the documented "overrides merge on top of the globals"
    /// contract (`RequiredFieldRule`'s "global plus per-kind override").
    pub fn required_for(&self, kind: &str) -> Vec<String> {
        let mut out = self.schema.required.clone();
        if let Some(ov) = self.schema_override_for(kind) {
            for field in &ov.required {
                if !out.contains(field) {
                    out.push(field.clone());
                }
            }
        }
        out
    }

    /// Every frontmatter field name that is *declared* for a given
    /// kind — built-in fields, plus every key referenced by `required`,
    /// `types`, `enums`, or `cross_field` (global + first matching
    /// override). For `cross_field` the set includes both the
    /// `require` target *and* the field named on the LHS of the
    /// `when` predicate, so a rule like
    /// `when = "priority=high" require = "owner"` implicitly declares
    /// `priority` — otherwise strict mode would reject the very
    /// documents the predicate is meant to fire on. Used by
    /// [`crate::rules::schema::UnknownFieldRule`].
    pub fn declared_fields_for(&self, kind: &str) -> std::collections::BTreeSet<String> {
        let mut out: std::collections::BTreeSet<String> = BUILTIN_FRONTMATTER_FIELDS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        for f in self.required_for(kind) {
            out.insert(f.clone());
        }
        for f in self.types_for(kind).keys() {
            out.insert(f.clone());
        }
        for f in self.enums_for(kind).keys() {
            out.insert(f.clone());
        }
        for cf in self.cross_field_for(kind) {
            let pred = parse_when(&cf.when).expect("validated by Config::load");
            out.insert(pred.field().to_string());
            out.insert(cf.require);
        }
        out
    }

    /// Union of [`Self::declared_fields_for`] across every kind in
    /// `kinds.allowed` plus the global schema (independent of kind).
    /// Used by validators that need a project-wide "is this field name
    /// known to *any* part of the schema?" question — for example,
    /// [`crate::config::RulesConfig::frontmatter_immutable`] rejects
    /// lock entries whose name is nowhere declared.
    pub fn declared_fields_universe(&self) -> std::collections::BTreeSet<String> {
        let mut out: std::collections::BTreeSet<String> = BUILTIN_FRONTMATTER_FIELDS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        // Global schema, independent of kind.
        for f in &self.schema.required {
            out.insert(f.clone());
        }
        for f in self.schema.types.keys() {
            out.insert(f.clone());
        }
        for f in self.schema.enums.keys() {
            out.insert(f.clone());
        }
        for cf in &self.schema.cross_field {
            let pred = parse_when(&cf.when).expect("validated by Config::load");
            out.insert(pred.field().to_string());
            out.insert(cf.require.clone());
        }
        // Plus every per-kind override (in case an override declares
        // fields that no global ever references).
        for kind in &self.kinds.allowed {
            out.extend(self.declared_fields_for(kind));
        }
        out
    }

    /// Find the schema override that applies to a given kind, if any.
    pub fn schema_override_for(&self, kind: &str) -> Option<&SchemaOverride> {
        self.schema
            .overrides
            .iter()
            .find(|ov| ov.kinds.iter().any(|k| k == kind))
    }

    /// Find the trust weight override that applies to a given kind.
    pub fn trust_weight_override_for(&self, kind: &str) -> Option<&TrustWeightOverride> {
        self.trust
            .overrides
            .iter()
            .find(|ov| ov.kinds.iter().any(|k| k == kind))
    }

    /// Merged trust weights for a kind — override replaces global
    /// entirely when matched. Parallels `required_for` / `types_for`
    /// / `enums_for` in taking a kind and returning the effective view.
    pub fn trust_weights_for(&self, kind: &str) -> TrustWeights {
        match self.trust_weight_override_for(kind) {
            Some(ov) => ov.weights,
            None => self.trust.weights,
        }
    }

    /// Every relation a *resolved* edge in this project can carry —
    /// [`crate::model::BUILTIN_EDGE_RELATIONS`] plus every
    /// `[[parser.link_patterns]].relation` the operator declared.
    ///
    /// Consumed by the surfaces that read resolved edges and take a
    /// user-supplied relation filter (`query dependents --relations …`,
    /// `impact --relations …`, `detection.git_drift_relations`,
    /// `rules.acyclic_relations`), so a typo surfaces as a typed error
    /// instead of silently matching zero edges — and so does a relation
    /// no resolved edge can carry, which would read the same.
    pub fn known_relations(&self) -> std::collections::BTreeSet<String> {
        self.relations_over(crate::model::BUILTIN_EDGE_RELATIONS)
    }

    /// Every relation an *unresolved* edge in this project can carry:
    /// [`known_relations`](Self::known_relations) plus the relations that
    /// exist only where resolution failed.
    ///
    /// `superseded_by` is the one of those — a resolved successor
    /// reference is materialised as a `supersedes` edge — and
    /// `[[detection.unresolved_policy]]` is the one surface that reads
    /// the unresolved plane, so it is the one place the wider vocabulary
    /// is in scope.
    pub fn unresolved_edge_relations(&self) -> std::collections::BTreeSet<String> {
        self.relations_over(crate::model::edge::EDGE_RELATIONS)
    }

    fn relations_over(&self, builtin: &[&str]) -> std::collections::BTreeSet<String> {
        let mut out: std::collections::BTreeSet<String> =
            builtin.iter().map(|s| (*s).to_string()).collect();
        for lp in &self.parser.link_patterns {
            out.insert(lp.relation.clone());
        }
        out
    }

    /// The status value tool-level actions (`scaffold`, `migrate`) write
    /// when they create a new document: the explicit `statuses.initial`
    /// when declared, otherwise the first `statuses.allowed` value.
    ///
    /// Kind-independent by design — a per-kind initial is an explicit
    /// concern, never inferred from the order of a `status` enum (a set,
    /// not a lifecycle ordering). `Config::validate` guarantees the
    /// result satisfies every declared `status` enum, so scaffold/migrate
    /// output always passes the same config's `check`.
    pub fn initial_status(&self) -> &str {
        resolve_initial_status(&self.statuses)
    }
}

/// Resolve the initial status for a freshly-created or frontmatter-less
/// document: the explicit `statuses.initial` when declared, otherwise the
/// first `statuses.allowed` value. Shared by [`Config::initial_status`]
/// and the parser so a scaffold and a frontmatter-less parse land on the
/// same default. Self-consistency against declared `status` enums is
/// enforced at load time by `Config::validate`, not re-derived here.
pub(crate) fn resolve_initial_status(statuses: &StatusesConfig) -> &str {
    match &statuses.initial {
        Some(initial) => initial.as_str(),
        None => statuses
            .allowed
            .first()
            .map(String::as_str)
            .expect("statuses.allowed non-empty — enforced by Config::validate"),
    }
}
