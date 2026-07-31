//! Load-time validation: `Config::validate` and its sub-validators,
//! plus the immutable-block helper they share. The single seam where a
//! config value is checked before any runtime consumer reads it.

use std::collections::BTreeMap;
use std::path::Path;

use super::predicate::*;
use super::types::*;
use super::views::resolve_initial_status;
use crate::error::{Error, Result};

/// Common view of an immutability-rule config block — owned by the
/// validator so the two families (`body_immutable`,
/// `frontmatter_immutable`) reject the same typos with the same
/// message shape. `fields` is `Some` only for `frontmatter_immutable`,
/// whose per-block payload is a frontmatter field list. Body
/// immutability has no field-list payload; its `mode` is enforced at
/// check time, not at config load.
struct ImmutableBlock<'a> {
    name: &'a str,
    fields: Option<&'a [String]>,
    kinds: &'a [String],
}

/// Refuse any immutability block whose `name`, kind filter, or
/// field-list (frontmatter only) would silently mis-fire at check
/// time.
fn validate_immutable_blocks<'a, I>(config: &Config, family: &str, blocks: I) -> Result<()>
where
    I: IntoIterator<Item = ImmutableBlock<'a>>,
{
    let mut seen_names: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let field_universe = config.declared_fields_universe();
    for (idx, block) in blocks.into_iter().enumerate() {
        if block.name.trim().is_empty() {
            return Err(Error::Config(format!(
                "{family}[{idx}].name must be a non-empty string"
            )));
        }
        if !seen_names.insert(block.name) {
            return Err(Error::Config(format!(
                "{family}[{idx}].name {:?} is declared more than once; \
                 names must be unique so violation rule_ids stay distinguishable",
                block.name
            )));
        }
        let ctx = format!("{family}[{idx}] ({name:?})", name = block.name);
        config.validate_kinds(&ctx, block.kinds)?;
        if let Some(fields) = block.fields {
            for field in fields {
                if field == "id" {
                    return Err(Error::Config(format!(
                        "{ctx}.fields contains \"id\", which cannot be locked here: \
                         `id` is the graph join key, so a present document cannot \
                         change its id without becoming a different node (and \
                         `rename` anchors it before moving). A diff cannot tell a \
                         genuine id change from a scope exclusion or an id-rule \
                         re-key, so the lock could only fire as a false positive — \
                         remove it. `id` immutability is structural, not a rule"
                    )));
                }
                if !field_universe.contains(field) {
                    return Err(Error::Config(format!(
                        "{ctx}.fields contains {field:?} which is neither a \
                         built-in frontmatter field nor declared in [schema] \
                         (required / types / enums / cross_field). Locking an \
                         unknown field would never fire — declare it or remove \
                         it from the lock list"
                    )));
                }
            }
            if fields.is_empty() {
                return Err(Error::Config(format!(
                    "{ctx}.fields must list at least one field — an empty list \
                     locks nothing and would silently never fire"
                )));
            }
        }
    }
    Ok(())
}

impl Config {
    /// Load config from a `nodex.toml` file. Returns default config if not found.
    ///
    /// Config is validated for internal consistency before it is returned,
    /// so downstream code can assume that `enums` / `cross_field` references
    /// are well-formed.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("nodex.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path).map_err(|e| Error::Io {
            path: path.clone(),
            source: e,
        })?;
        let config: Self =
            toml::from_str(&content).map_err(|e| Error::Config(format!("{path:?}: {e}")))?;
        config.validate()?;
        Ok(config)
    }

    /// True when any diff-aware immutability rule is configured. Lets a
    /// caller decide whether resolving an `immutable_baseline` diff (a
    /// worktree build) is worth doing — with no immutability rules the
    /// diff would feed nothing.
    pub fn has_immutable_rules(&self) -> bool {
        !self.rules.frontmatter_immutable.is_empty() || !self.rules.body_immutable.is_empty()
    }

    /// Validate internal consistency. Called automatically by `load()`.
    ///
    /// Rejects definitions that would otherwise only surface as
    /// confusing runtime behaviour:
    /// - `enums` on collection-valued built-in fields (`tags`,
    ///   `supersedes`, `implements`, `related`, `covers`) — these
    ///   cannot be validated against a scalar set, so silent ignore
    ///   would trap users who typed the obvious syntax and saw no
    ///   effect.
    /// - `enums.status` / `enums.kind` values that are not in the
    ///   corresponding global `allowed` list.
    /// - `cross_field.when` expressions that don't parse.
    /// - `cross_field.when`'s LHS and `cross_field.require` referring
    ///   to a field name that is not a built-in and is not declared in
    ///   the kind's MERGED `types` / `enums` / `required` (global +
    ///   override), so a predicate may name a field declared in any block
    ///   that contributes to that kind.
    /// - `equals` / `in` predicates on collection-valued fields — these
    ///   always evaluate false; `exists` / `not_exists` should be used
    ///   instead.
    pub fn validate(&self) -> Result<()> {
        self.validate_meta()?;
        self.validate_vocabulary()?;
        self.validate_detection()?;
        self.validate_output()?;
        self.validate_report()?;
        self.validate_scope()?;
        self.validate_block(
            "schema",
            &self.schema.required,
            &self.schema.types,
            &self.schema.enums,
            &self.schema.cross_field,
        )?;
        self.validate_require_explicit()?;
        // `cross_field.when` syntax up front, before any consumer:
        // `validate_immutability` reaches `declared_fields_*`, which parse
        // `when` with `.expect("validated by Config::load")`. This pass is
        // what makes that expectation true (field RESOLUTION stays in
        // `validate_merged_cross_fields`, which needs the merged view).
        self.validate_cross_field_syntax()?;
        self.validate_identity()?;
        self.validate_extraction()?;
        self.validate_relations()?;
        self.validate_immutability()?;
        self.validate_scoring()?;
        self.validate_schema_overrides()?;
        self.validate_merged_enum_satisfiability()?;
        self.validate_merged_field_enums()?;
        self.validate_merged_cross_fields()?;
        Ok(())
    }

    /// Parse every `cross_field.when` (global + each override) so no
    /// post-validate consumer can see an unparsed predicate. Every such
    /// consumer reads it through `.expect("validated by Config::load")`
    /// — `declared_fields_for` / `declared_fields_universe` (reached
    /// during `validate_immutability` and at runtime), the
    /// `CrossFieldRule` check pass, scaffold's default renderer, and
    /// the lifecycle write-seam guard — one uniform contract, never a
    /// silent skip.
    fn validate_cross_field_syntax(&self) -> Result<()> {
        for cf in &self.schema.cross_field {
            parse_when(&cf.when).map_err(|e| {
                Error::Config(format!("schema: cross_field.when {:?}: {e}", cf.when))
            })?;
        }
        for (idx, ov) in self.schema.overrides.iter().enumerate() {
            for cf in &ov.cross_field {
                parse_when(&cf.when).map_err(|e| {
                    Error::Config(format!(
                        "schema.overrides[{idx}]: cross_field.when {:?}: {e}",
                        cf.when
                    ))
                })?;
            }
        }
        Ok(())
    }

    /// Validate each `cross_field` against the MERGED per-kind view — the
    /// view `check`'s `CrossFieldRule` consumes via `cross_field_for`. A
    /// global predicate applies to every kind, an override's only to its
    /// kinds; either may legitimately reference a field declared in the
    /// OTHER block, so a predicate's `when` / `require` fields must
    /// resolve against the kind's merged declarations (global plus
    /// override plus built-ins), never one block in isolation — a
    /// block-local check would false-reject an override predicate
    /// naming a globally-declared field and silently admit a global
    /// predicate naming a field only some kinds declare.
    fn validate_merged_cross_fields(&self) -> Result<()> {
        for kind in &self.kinds.allowed {
            let required = self.required_for(kind);
            let types = self.types_for(kind);
            let enums = self.enums_for(kind);
            // The predicate-value check must read the same effective enum
            // view the runtime `FieldEnumRule` enforces (declared enums +
            // backfilled kind/status), so a `when` value the narrowed enum
            // excludes is a load error here exactly when `check` would
            // reject it. One backfill seam in `config/views.rs` — never a
            // copy that can drift. (`enums` stays the raw *declared* view
            // for `ensure_field_known`, which tests declaration, not value.)
            let predicate_enums = self.effective_enums_for(kind);
            let ctx = format!("cross_field for kind {kind:?}");
            for cf in self.cross_field_for(kind) {
                let predicate = parse_when(&cf.when)
                    .map_err(|e| Error::Config(format!("{ctx}: when {:?}: {e}", cf.when)))?;
                let when_field = predicate.field();
                ensure_field_known(when_field, &required, &types, &enums, &ctx, "when")?;
                if is_collection_builtin(when_field)
                    && matches!(
                        predicate,
                        WhenPredicate::Equals { .. } | WhenPredicate::In { .. }
                    )
                {
                    return Err(Error::Config(format!(
                        "{ctx}: when references collection field {when_field:?}; equals/in \
                         predicates operate on scalar values — use exists/not_exists for \
                         collection presence"
                    )));
                }
                // The `when` value(s) must be values the field can actually
                // hold, or the predicate is accepted at load yet never fires
                // — the silent runtime skip `.claude/rules/config-driven.md`
                // forbids. This is the value half of the same intent
                // `parse_when` already enforces structurally (it rejects
                // `==` / leading `=` typos).
                ensure_predicate_values_match_field(&predicate, &types, &predicate_enums, &ctx)?;
                ensure_field_known(&cf.require, &required, &types, &enums, &ctx, "require")?;
                // A `require` naming a parser-resolved field
                // (`INFERRED_FRONTMATTER_FIELDS` — the same vocabulary
                // the `required` guard in `validate_block` derives
                // from) can never read as missing post-inference, so
                // the entry would be accepted-but-inert. `orphan_ok`
                // is exempt: its pass-once-declared semantics are a
                // deliberate contract (`is_field_missing` treats the
                // boolean as structurally present — rules/schema.rs),
                // so the exemption keeps `require = "orphan_ok"` legal
                // alongside the documented predicate behaviour.
                // `when = "status=…"` predicates stay legal throughout
                // — they read values, not presence.
                if cf.require != "orphan_ok"
                    && INFERRED_FRONTMATTER_FIELDS.contains(&cf.require.as_str())
                {
                    return Err(Error::Config(format!(
                        "{ctx}: require names {:?}, a field the parser always resolves \
                         (id/kind from identity rules, status from statuses.initial, title \
                         from the H1 or filename stem) — the predicate could never fire. \
                         Drop it; presence is guaranteed by construction, and value \
                         constraints belong in types / enums",
                        cf.require
                    )));
                }
                // Self-consistency: a `require` field must accept a
                // tool-generated default that the same rule then accepts,
                // or scaffold/migrate would write a document that fails
                // this very rule on the next check.
                ensure_cross_field_default_satisfiable(&cf.require, &types, &enums, &ctx)?;
            }
        }
        Ok(())
    }

    /// Validate the `type`/`enum` interaction on the MERGED per-kind view
    /// — the view `check`'s field rules and `export_schema` actually
    /// consume — not just within a single `[schema]` / `[[schema.overrides]]`
    /// block. `types_for` / `enums_for` overlay an override onto the
    /// global block, so a constraint split across the two (a `bool` type
    /// in `[schema]` and an enum in an override, or a typed field and a
    /// type-incompatible enum) passes each block's local view yet
    /// combines into a constraint the export emits inconsistently. Two
    /// invariants, each checked against the merged view of every kind so
    /// the split path closes exactly like the same-block path:
    ///
    /// - An enum on a boolean field is meaningless (a `bool` already
    ///   permits exactly `true`/`false`) and ill-defined in the export
    ///   (string enum values vs a `boolean` JSON type).
    /// - Every enum value must parse as the field's declared type, or
    ///   `scaffold` would write its first-value default and the next
    ///   `check` would immediately flag it.
    fn validate_merged_field_enums(&self) -> Result<()> {
        for kind in &self.kinds.allowed {
            let types = self.types_for(kind);
            for (field, allowed) in self.enums_for(kind) {
                let is_bool =
                    field == "orphan_ok" || matches!(types.get(&field), Some(FieldType::Bool));
                if is_bool {
                    return Err(Error::Config(format!(
                        "enums.{field} applies to a boolean field (kind {kind:?}): a boolean \
                         field already permits exactly true/false, so an enum on it is \
                         redundant — drop the enum, or use a cross_field rule to require a \
                         fixed value (check whether [schema] and a [[schema.overrides]] split \
                         the type and the enum)"
                    )));
                }
                if let Some(ty) = types.get(&field)
                    && let Some(bad) = allowed.iter().find(|v| !value_matches_field_type(v, *ty))
                {
                    return Err(Error::Config(format!(
                        "enums.{field} value {bad:?} is not a valid {ty:?} (kind {kind:?}); \
                         either drop the enum or widen the type (check whether [schema] and a \
                         [[schema.overrides]] split the type and the enum)"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Two self-consistency invariants on the MERGED per-kind `enums`
    /// view — the view `check`'s field_enum rule and `scaffold` consume —
    /// so a global enum shadowed by an override for every kind neither
    /// over- nor under-rejects. Runs after the block validators so the
    /// merged views are well-formed.
    ///
    /// - Every kind must be admitted by its own merged `kind` enum: a
    ///   declared `kind` enum *replaces* the `kinds.allowed` back-fill, so
    ///   a view omitting a kind it governs makes that kind unsatisfiable
    ///   (every document of it fails `check` forever, and the exported
    ///   schema branch would disagree).
    /// - The effective initial status `scaffold` / `migrate` write must be
    ///   admitted by every kind's merged `status` enum, or a tool-written
    ///   document fails the config's own `field_enum`.
    fn validate_merged_enum_satisfiability(&self) -> Result<()> {
        let initial_status = resolve_initial_status(&self.statuses);
        for kind in &self.kinds.allowed {
            let enums = self.enums_for(kind);
            if let Some(kind_enum) = enums.get("kind")
                && !kind_enum.iter().any(|v| v == kind)
            {
                return Err(Error::Config(format!(
                    "enums.kind {kind_enum:?} applies to kind {kind:?} but does not list it; \
                     every document of that kind would fail field_enum — add the kind to the \
                     enum or drop the entry"
                )));
            }
            if let Some(status_enum) = enums.get("status")
                && !status_enum.iter().any(|v| v == initial_status)
            {
                return Err(Error::Config(format!(
                    "initial status {initial_status:?} (statuses.initial, else the first \
                     statuses.allowed) is not permitted by the status enum for kind {kind:?}; \
                     declare a statuses.initial every kind's status enum allows"
                )));
            }
        }
        Ok(())
    }

    /// `meta.nodex_version` must be a valid SemVer requirement — a bad
    /// pin is a config bug regardless of which binary reads it, so the
    /// question at every command boundary stays "does this binary
    /// satisfy the (valid) pin?".
    fn validate_meta(&self) -> Result<()> {
        if let Some(req) = self.meta.nodex_version.as_deref() {
            semver::VersionReq::parse(req).map_err(|e| {
                Error::Config(format!(
                    "meta.nodex_version {req:?} is not a valid SemVer requirement: {e}"
                ))
            })?;
        }
        Ok(())
    }

    /// Kind and status vocabulary: `kinds.allowed` / `statuses.allowed`
    /// non-empty, `statuses.terminal` and `statuses.initial` ⊆ allowed,
    /// the effective initial status permitted by every declared status
    /// enum, the `FALLBACK_KIND` present, and `orphan_ok_kinds` ⊆
    /// allowed — the self-consistency invariant for everything the tool
    /// writes by default.
    fn validate_vocabulary(&self) -> Result<()> {
        // A duplicated vocabulary entry is always a config typo, and it
        // leaks: `export schema` / `export enums` emit these lists as
        // JSON-Schema `enum` arrays, whose elements draft 2020-12
        // requires to be unique — a strict downstream validator would
        // reject the exported contract. The guard policy across every
        // config list: duplicates are rejected wherever they change an
        // output (exported arrays, violation counts, extracted edges);
        // pure-membership lists (scope globs, orphan_ok_kinds,
        // extensions, stop words, per-rule kinds filters) tolerate them
        // because a duplicate there is provably inert.
        for (list, name) in [
            (&self.kinds.allowed, "kinds.allowed"),
            (&self.statuses.allowed, "statuses.allowed"),
            (&self.statuses.terminal, "statuses.terminal"),
        ] {
            let mut seen = std::collections::BTreeSet::new();
            if let Some(dup) = list.iter().find(|v| !seen.insert(v.as_str())) {
                return Err(Error::Config(format!(
                    "{name} lists {dup:?} more than once — drop the duplicate"
                )));
            }
        }

        // Refuse structurally-broken configs: empty `kinds.allowed`
        // means every document would be kind-less (inference falls
        // back to "generic") yet no kind would ever be valid — either
        // the user is mis-configured or they meant "accept all kinds"
        // (which is the default when the key is omitted entirely).
        if self.kinds.allowed.is_empty() {
            return Err(Error::Config(
                "kinds.allowed must not be empty; omit the key to accept the defaults, \
                 or list every kind your project uses"
                    .to_string(),
            ));
        }

        // Same rationale as `kinds.allowed`: an empty `statuses.allowed`
        // would make every status value invalid and break scaffolding,
        // which picks the first allowed status for the initial value.
        if self.statuses.allowed.is_empty() {
            return Err(Error::Config(
                "statuses.allowed must not be empty; omit the key to accept the defaults, \
                 or list every status your project uses"
                    .to_string(),
            ));
        }

        // `statuses.terminal` drives `is_terminal`, which gates
        // body / frontmatter immutability rules and decides which
        // statuses block further lifecycle transitions. A terminal
        // entry that is not in `statuses.allowed` is the "tool writes
        // a value the same config rejects" failure mode in two
        // different ways at once: a `lifecycle set --status` onto the
        // mis-spelled terminal would be refused at the write seam (the
        // doc silently never terminates), and any node that *did* land
        // on the typo'd status would later fail FieldEnumRule. Refuse
        // at load.
        for status in &self.statuses.terminal {
            if !self.statuses.allowed.iter().any(|s| s == status) {
                return Err(Error::Config(format!(
                    "statuses.terminal contains {status:?} which is not in statuses.allowed; \
                     every terminal status must also be in allowed. statuses.terminal defaults \
                     to {default:?} when omitted, so a narrowed statuses.allowed needs \
                     statuses.terminal declared explicitly — list the terminal statuses you \
                     keep, or `terminal = []` for none.",
                    default = default_terminal()
                )));
            }
        }

        if let Some(initial) = &self.statuses.initial
            && !self.statuses.allowed.iter().any(|s| s == initial)
        {
            return Err(Error::Config(format!(
                "statuses.initial is {initial:?} but not in statuses.allowed; \
                 initial status must be a valid allowed status"
            )));
        }

        // The effective initial status (statuses.initial, else the first
        // allowed) must satisfy every kind's MERGED status enum — checked
        // in `validate_merged_enum_satisfiability` after the override
        // blocks are validated, against the same `enums_for` view scaffold
        // and check consume.

        // `FALLBACK_KIND` is what `parser::identity::infer_kind`
        // assigns when no `identity.kind_rules` glob matches a
        // document's path, and what `migrate` injects when scaffolding
        // frontmatter onto a bare file. Leaving this out of
        // `kinds.allowed` was the exact defect that let `migrate` /
        // `parse_document` write documents their own config then
        // rejected. Require its presence at load; projects that want
        // every document strongly classified can still write
        // exhaustive `kind_rules`, in which case the fallback simply
        // never fires.
        if !self
            .kinds
            .allowed
            .iter()
            .any(|k| k == crate::parser::identity::FALLBACK_KIND)
        {
            return Err(Error::Config(format!(
                "kinds.allowed is missing the required fallback kind {fallback:?}. \
                 \n\nWhy? Every document must have a kind. When no identity.kind_rules glob matches \
                 a file's path, {fallback:?} is assigned as the catch-all kind. Without it, \
                 the parser would fail on unclassified documents. \
                 \n\nHow to fix: \
                 \n  Option 1 (recommended): Add {fallback:?} to kinds.allowed: \
                 \n    kinds.allowed = [\"adr\", \"guide\", {fallback:?}, ...] \
                 \n  Option 2: Remove kinds.allowed entirely to use defaults (includes {fallback:?}): \
                 \n    # kinds.allowed is omitted, using built-in defaults \
                 \n\nAlternatively, declare exhaustive identity.kind_rules to classify all documents, \
                 in which case {fallback:?} becomes a safety net that never fires.",
                fallback = crate::parser::identity::FALLBACK_KIND
            )));
        }

        // Every `detection.orphan_ok_kinds` entry must reference a kind
        // the project actually accepts; a typo would otherwise load
        // cleanly and the runtime would exempt nothing.
        for k in &self.detection.orphan_ok_kinds {
            if !self.kinds.allowed.iter().any(|a| a == k) {
                return Err(Error::Config(format!(
                    "detection.orphan_ok_kinds contains {k:?} which is not in \
                     kinds.allowed; add it to kinds.allowed or remove the exemption"
                )));
            }
        }
        Ok(())
    }

    /// `[detection]` numeric guards: thresholds are strictly positive
    /// or `None` — zero is ambiguous between "off" and "immediate", so
    /// it is refused at load. `[[detection.unresolved_policy]]` rows:
    /// names unique and non-reserved, globs compile and only appear on
    /// path-carrying causes, no duplicate `(cause, glob)` pair — the
    /// same "no silent runtime skips" discipline as every other
    /// per-block rule family.
    fn validate_detection(&self) -> Result<()> {
        if let Some(0) = self.detection.stale_days {
            return Err(Error::Config(
                "detection.stale_days must be > 0 or None (omitted to disable); got 0".to_string(),
            ));
        }

        if let Some(0) = self.detection.git_drift_threshold {
            return Err(Error::Config(
                "detection.git_drift_threshold must be > 0 or None (omitted to disable); got 0"
                    .to_string(),
            ));
        }

        // Declaring the table replaces the default policy entirely, so
        // an explicit `[]` is a contradiction: it looks like a
        // configured policy but configures nothing (every edge would
        // take the warning fallthrough). The default never reaches
        // here empty.
        if self.detection.unresolved_policy.is_empty() {
            return Err(Error::Config(
                "detection.unresolved_policy is empty — omit the key to take the default \
                 ({name = \"excluded_target\", cause = \"excluded_from_scope\", \
                 severity = \"info\"}), or declare at least one row"
                    .to_string(),
            ));
        }
        use crate::model::edge::categories;
        // Info rows count under `by_category[<name>]` in the same map
        // as these built-in keys — a colliding name would make one
        // count unreadable as the other.
        let reserved = [
            categories::ORPHAN,
            categories::STALE,
            categories::UNRESOLVED_EDGE,
        ];
        let mut policy_names = std::collections::BTreeSet::new();
        let mut policy_pairs: Vec<(crate::model::UnresolvedCause, Option<&str>)> = Vec::new();
        for (idx, row) in self.detection.unresolved_policy.iter().enumerate() {
            // The TOML vocabulary spelling (snake_case), for messages
            // that echo what the user wrote.
            let cause_name = serde_json::to_value(row.cause)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .expect("UnresolvedCause serializes as a string");
            if row.name.trim().is_empty() {
                return Err(Error::Config(format!(
                    "detection.unresolved_policy[{idx}].name must be a non-empty string"
                )));
            }
            if reserved.contains(&row.name.as_str())
                || row.name.starts_with(categories::VIOLATION_PREFIX)
            {
                return Err(Error::Config(format!(
                    "detection.unresolved_policy[{idx}].name {:?} is a reserved issue-summary \
                     category ({reserved:?} and the \"{prefix}\" prefix); pick another name",
                    row.name,
                    prefix = categories::VIOLATION_PREFIX,
                )));
            }
            if !policy_names.insert(row.name.as_str()) {
                return Err(Error::Config(format!(
                    "detection.unresolved_policy[{idx}].name {:?} is declared more than once; \
                     names must be unique so per-edge attribution and violation rule_ids stay \
                     distinguishable",
                    row.name
                )));
            }
            if let Some(glob) = row.glob.as_deref() {
                if !row.cause.has_path_candidates() {
                    return Err(Error::Config(format!(
                        "detection.unresolved_policy[{idx}] ({name:?}) declares a glob, but \
                         cause {cause_name:?} carries no path candidates to match it against \
                         (ids and resolution-time refusals are pathless); remove the glob or \
                         use a path-carrying cause",
                        name = row.name,
                    )));
                }
                globset::Glob::new(glob).map_err(|e| {
                    Error::Config(format!(
                        "detection.unresolved_policy[{idx}] ({name:?}).glob {glob:?} is not a \
                         valid glob: {e}",
                        name = row.name,
                    ))
                })?;
            }
            let pair = (row.cause, row.glob.as_deref());
            if policy_pairs.contains(&pair) {
                return Err(Error::Config(format!(
                    "detection.unresolved_policy[{idx}] ({name:?}) duplicates an earlier row's \
                     (cause = {cause_name:?}, glob = {glob:?}) pair — first match wins, so the \
                     later row can never fire; drop one",
                    name = row.name,
                    glob = row.glob,
                )));
            }
            policy_pairs.push(pair);
        }
        Ok(())
    }

    /// `[output]` containment, lexical half: `output.dir` is joined to
    /// the project root whenever build / report / cache write their
    /// artefacts, so a value like `"../escape"` or `"/etc/out"` is
    /// refused at load for early feedback
    /// (`path_guard::reject_traversal`, the same guard user-supplied
    /// paths get on rename / scaffold / migrate). The filesystem half —
    /// a lexically clean dir that resolves outside the root through a
    /// symlinked ancestor — is only detectable at write time and is a
    /// property of the write primitive itself
    /// (`path_guard::write_atomic_in_root`), so no handler can opt out.
    /// An empty value is refused outright, so the traversal guard and
    /// the scanner's GRAPH.md self-exclusion run unconditionally
    /// downstream.
    fn validate_output(&self) -> Result<()> {
        crate::path_guard::reject_traversal(std::path::Path::new(&self.output.dir)).map_err(
            |_| {
                Error::Config(format!(
                    "output.dir {:?} escapes the project root; \
                     use a relative path without `..` or a leading `/`",
                    self.output.dir
                ))
            },
        )?;

        // `.` and `./` name the project root as surely as an empty value
        // does, and the consequence is worse than a re-scanned GRAPH.md: the
        // self-exclusion covers the whole project, so the scan yields nothing.
        if crate::path_guard::normalize_relative(std::path::Path::new(&self.output.dir))
            .is_none_or(|rel| rel.is_empty())
        {
            return Err(Error::Config(format!(
                "output.dir {:?} names the project root — artefacts would land there, the \
                 GRAPH.md self-exclusion would cover the whole project, and the scan would \
                 yield no documents. Omit output.dir to take the \"_index\" default, or name a \
                 directory",
                self.output.dir
            )));
        }
        Ok(())
    }

    /// `[report]` display limits: every `*_display_limit` selects how many
    /// entries a section of `GRAPH.md` renders, so `0` produces a
    /// degenerate section (a blank god-node list, or a `…and N more` line
    /// for every entry). Reject it at load like every other count knob
    /// (`similarity.default_limit`), so a useless value can never reach
    /// the renderer — the report block joins the comprehensive load-time
    /// validation every other block already gets.
    fn validate_report(&self) -> Result<()> {
        for (field, value) in [
            ("god_node_display_limit", self.report.god_node_display_limit),
            ("orphan_display_limit", self.report.orphan_display_limit),
            ("stale_display_limit", self.report.stale_display_limit),
        ] {
            if value == 0 {
                return Err(Error::Config(format!(
                    "report.{field} must be ≥ 1 — `0` renders an empty section"
                )));
            }
        }
        Ok(())
    }

    /// `[scope]` glob surface: reject an empty include list and compile
    /// every include / effective-exclude pattern at load through the
    /// scanner's own compile path (`scanner::build_globset` over the
    /// `ScanConfig` projection), so load-accept implies scan-success
    /// with zero message drift — the scanner stays the single runtime
    /// authority, and the derived output-dir self-exclusion glob is
    /// covered exactly as a scan would compile it.
    fn validate_scope(&self) -> Result<()> {
        // An explicit `include = []` scans nothing — every command then
        // sees an empty graph and `check` passes on an unscanned corpus.
        // Omit `include` to take the `**/*.md` default; never write `[]`.
        if self.scope.include.is_empty() {
            return Err(Error::Config(
                "scope.include is empty — it would match no files and silently empty the \
                 graph (check would pass on an unscanned project). Omit scope.include to take \
                 the \"**/*.md\" default, or list the globs your project scans"
                    .to_string(),
            ));
        }
        let scan = crate::builder::scanner::ScanConfig::new(self);
        crate::builder::scanner::build_globset(&self.scope.include, "scope.include")?;
        crate::builder::scanner::build_globset(
            &scan.effective_exclude_patterns(),
            "scope.exclude",
        )?;
        // `prune_dirs` are plain directory basenames matched at any depth
        // — the walk-time prune is a cheap segment compare, so an entry
        // must be a single non-empty basename (no path separators, no
        // glob metacharacters — use `scope.exclude` for glob exclusion)
        // and never duplicated. An empty list is legal: it prunes
        // nothing (dot-prefixed trees stay caught by the hidden guard).
        let mut seen_prune = std::collections::BTreeSet::new();
        for dir in &self.scope.prune_dirs {
            if dir.trim().is_empty() {
                return Err(Error::Config(
                    "scope.prune_dirs has an empty entry — list directory basenames to prune"
                        .to_string(),
                ));
            }
            if dir.contains('/') || dir.contains('\\') {
                return Err(Error::Config(format!(
                    "scope.prune_dirs entry {dir:?} contains a path separator — list a plain \
                     directory basename (matched at any depth), not a path"
                )));
            }
            if dir.contains(['*', '?', '[', ']']) {
                return Err(Error::Config(format!(
                    "scope.prune_dirs entry {dir:?} contains a glob metacharacter — pruning is a \
                     plain segment match; use scope.exclude for glob-based exclusion"
                )));
            }
            if !seen_prune.insert(dir.as_str()) {
                return Err(Error::Config(format!(
                    "scope.prune_dirs lists {dir:?} more than once — drop the duplicate"
                )));
            }
        }
        Ok(())
    }

    /// `[identity]` and `[[rules.naming]]`: compile every glob/regex the
    /// runtime depends on, and check each `kind_rules` / `id_rules` kind
    /// against `kinds.allowed` and each id template against the known
    /// placeholder set — a pattern that loads but cannot run downstream
    /// would silently make a rule never fire.
    fn validate_identity(&self) -> Result<()> {
        // Pre-validate every glob and regex the runtime depends on.
        // The contract is symmetric: the load-time validator's only
        // purpose is to reject what the runtime cannot honour, and the
        // runtime never silently skips a rule the validator accepted.
        // Both halves break if a pattern that loads cleanly fails to
        // compile downstream — projects then see "no violations" when
        // the truth is "no rule ever ran".
        // An identical naming block declared twice would fire twice and
        // report every violation in duplicate — a typo, reject it.
        let mut seen_naming = std::collections::BTreeSet::new();
        for (idx, nr) in self.rules.naming.iter().enumerate() {
            if !seen_naming.insert((
                nr.glob.as_str(),
                nr.pattern.as_str(),
                nr.sequential,
                nr.unique,
            )) {
                return Err(Error::Config(format!(
                    "rules.naming[{idx}] duplicates an earlier identical block — drop one"
                )));
            }
            globset::Glob::new(&nr.glob).map_err(|e| {
                Error::Config(format!(
                    "rules.naming[{idx}].glob {:?} is not a valid glob: {e}",
                    nr.glob
                ))
            })?;
            regex::Regex::new(&nr.pattern).map_err(|e| {
                Error::Config(format!(
                    "rules.naming[{idx}].pattern {:?} is not a valid regex: {e}",
                    nr.pattern
                ))
            })?;
        }
        for (idx, kr) in self.identity.kind_rules.iter().enumerate() {
            globset::Glob::new(&kr.glob).map_err(|e| {
                Error::Config(format!(
                    "identity.kind_rules[{idx}].glob {:?} is not a valid glob: {e}",
                    kr.glob
                ))
            })?;
            if !self.kinds.allowed.iter().any(|a| a == &kr.kind) {
                return Err(Error::Config(format!(
                    "identity.kind_rules[{idx}].kind {:?} is not in kinds.allowed",
                    kr.kind
                )));
            }
        }
        for (idx, ir) in self.identity.id_rules.iter().enumerate() {
            if let Some(glob) = &ir.glob {
                globset::Glob::new(glob).map_err(|e| {
                    Error::Config(format!(
                        "identity.id_rules[{idx}].glob {glob:?} is not a valid glob: {e}"
                    ))
                })?;
            }
            // `parser::identity::infer_kind` skips id_rules whose `kind`
            // is neither `*` nor the inferred kind. A value outside
            // `kinds.allowed` would silently never match — the rule
            // loads cleanly and the runtime applies it to nothing.
            // Refuse at load instead, matching the same subset
            // discipline as `identity.kind_rules[].kind` above.
            if ir.kind != "*" && !self.kinds.allowed.iter().any(|a| a == &ir.kind) {
                return Err(Error::Config(format!(
                    "identity.id_rules[{idx}].kind {:?} is not in kinds.allowed; \
                     use \"*\" for any-kind or one of the allowed kinds",
                    ir.kind
                )));
            }
            // `parser::identity::expand_template` only substitutes the
            // names listed in `ID_TEMPLATE_PLACEHOLDERS`; an unknown
            // placeholder (typo like `{stme}`) silently survives the
            // substitution and ends up literal in every generated id.
            // Reject any `{ident}` that isn't a recognised placeholder
            // at load — keeping validation in lockstep with the
            // substitution arms (no silent runtime skips).
            for placeholder in scan_template_placeholders(&ir.template) {
                if !ID_TEMPLATE_PLACEHOLDERS.contains(&placeholder.as_str()) {
                    return Err(Error::Config(format!(
                        "identity.id_rules[{idx}].template references unknown placeholder {placeholder:?}; \
                         valid placeholders: {{kind}}, {{stem}}, {{parent}}, {{path_slug}}"
                    )));
                }
            }
            // After accepting every well-formed `{ident}`, any leftover
            // `{` or `}` is a malformed brace: whitespace inside
            // (`{ kind }`), an unmatched brace (`{kind`, `kind}`), or a
            // double-brace (`{{kind}}`). `expand_template` would emit
            // every such fragment literal into the generated id — the
            // exact "no silent runtime skips" failure mode this
            // validator exists to refuse.
            if scan_template_malformed_braces(&ir.template) {
                return Err(Error::Config(format!(
                    "identity.id_rules[{idx}].template {template:?} contains malformed brace syntax; \
                     placeholders must be exactly {{kind}} / {{stem}} / {{parent}} / {{path_slug}} \
                     with no whitespace, no unmatched braces, and no double-brace escape",
                    template = ir.template,
                )));
            }
        }
        Ok(())
    }

    /// Body-extraction and scope patterns — `scope.conditional_exclude`,
    /// `parser.link_patterns` (exactly one capture group; relation never
    /// one whose resolution mode is code-fixed: the path-only `covers`
    /// or the id-resolved `supersedes` / `implements` / `related`),
    /// `rules.body_line` and `[[annotations]]`
    /// (unique names, compiled regex, every enum/key capture present,
    /// valid `kinds`), and `parser.extensions` (non-empty, dot-prefixed).
    /// All under the same "no silent runtime skips" discipline as the
    /// rest of the loader.
    fn validate_extraction(&self) -> Result<()> {
        for (idx, ce) in self.scope.conditional_exclude.iter().enumerate() {
            globset::Glob::new(&ce.parent_glob).map_err(|e| {
                Error::Config(format!(
                    "scope.conditional_exclude[{idx}].parent_glob {:?} is not a valid glob: {e}",
                    ce.parent_glob
                ))
            })?;
            globset::Glob::new(&ce.child_glob).map_err(|e| {
                Error::Config(format!(
                    "scope.conditional_exclude[{idx}].child_glob {:?} is not a valid glob: {e}",
                    ce.child_glob
                ))
            })?;
            // `builder::scanner::apply_conditional_excludes` only
            // honours `condition = "status_terminal"`; any other value
            // is silently skipped, which would make the rule load
            // cleanly and exclude nothing. Reject unknown conditions
            // at load so a typo surfaces with the valid set in the
            // error message.
            if !CONDITIONAL_EXCLUDE_CONDITIONS
                .iter()
                .any(|c| *c == ce.condition)
            {
                return Err(Error::Config(format!(
                    "scope.conditional_exclude[{idx}].condition {value:?} is unknown; \
                     valid values: {valid:?}",
                    value = ce.condition,
                    valid = CONDITIONAL_EXCLUDE_CONDITIONS,
                )));
            }
        }
        // An identical (pattern, relation) pair declared twice would
        // extract every matching reference twice — duplicated edges in
        // the graph. A typo, reject it.
        let mut seen_patterns = std::collections::BTreeSet::new();
        for (idx, lp) in self.parser.link_patterns.iter().enumerate() {
            if !seen_patterns.insert((lp.pattern.as_str(), lp.relation.as_str())) {
                return Err(Error::Config(format!(
                    "parser.link_patterns[{idx}] duplicates an earlier identical \
                     (pattern, relation) block — drop one"
                )));
            }
            let re = regex::Regex::new(&lp.pattern).map_err(|e| {
                Error::Config(format!(
                    "parser.link_patterns[{idx}].pattern {:?} is not a valid regex: {e}",
                    lp.pattern
                ))
            })?;
            // `parser::body` reads edge targets from `caps.get(1)` — the
            // first (and only) capture group. Each pattern must have
            // exactly one capture group to avoid silent misbehavior.
            // `captures_len()` counts group 0 (the full match) plus
            // every explicit `(...)` group, so a value of 2 means one
            // capture group was declared.
            match re.captures_len() {
                0 | 1 => {
                    return Err(Error::Config(format!(
                        "parser.link_patterns[{idx}].pattern {pattern:?} has no capture group; \
                         add exactly one (...) so link targets can be extracted",
                        pattern = lp.pattern,
                    )));
                }
                2 => {
                    // Expected: exactly one capture group
                }
                _ => {
                    return Err(Error::Config(format!(
                        "parser.link_patterns[{idx}].pattern {pattern:?} has multiple capture groups; \
                         only the first capture group is used, so having more is confusing. \
                         Use a single (...) group for the link target.",
                        pattern = lp.pattern,
                    )));
                }
            }
            // Resolution semantics attach to the frontmatter field that
            // produces a relation, never to a name a user can pick:
            // `covers` resolves path-only and `supersedes` /
            // `implements` / `related` resolve id-only, both fixed in
            // code. Body link patterns always resolve as document
            // references (extension-append + id-fallback), so a pattern
            // naming any relation in the closed code-owned set would
            // silently change how its targets bind — rejected at load.
            // `references` stays legal: document-reference mode is its
            // mode, so a pattern naming it shifts no semantics.
            let fixed_resolution = if lp.relation == crate::model::edge::PATH_ONLY_RELATION {
                Some("path-only")
            } else if crate::model::edge::ID_RESOLVED_RELATIONS.contains(&lp.relation.as_str()) {
                Some("id-resolved")
            } else {
                None
            };
            if let Some(mode) = fixed_resolution {
                return Err(Error::Config(format!(
                    "parser.link_patterns[{idx}].relation {relation:?} is the built-in {mode} \
                     relation, fed exclusively by the frontmatter {relation}: field — body \
                     link patterns resolve as document references and cannot emit it; declare \
                     the relation through that frontmatter field, or pick a different relation \
                     name for the pattern",
                    relation = lp.relation,
                )));
            }
        }

        // Body-line rules: compile, enum keys ∈ named captures, kinds
        // valid, names unique, `enums` non-empty. Same "no silent
        // runtime skips" discipline.
        let mut body_line_names: std::collections::BTreeSet<&str> =
            std::collections::BTreeSet::new();
        for (idx, bl) in self.rules.body_line.iter().enumerate() {
            if bl.name.trim().is_empty() {
                return Err(Error::Config(format!(
                    "rules.body_line[{idx}].name must be a non-empty string"
                )));
            }
            if !body_line_names.insert(bl.name.as_str()) {
                return Err(Error::Config(format!(
                    "rules.body_line[{idx}].name {:?} is declared more than once; \
                     names must be unique so violation rule_ids stay distinguishable",
                    bl.name
                )));
            }
            let re = regex::Regex::new(&bl.pattern).map_err(|e| {
                Error::Config(format!(
                    "rules.body_line[{idx}] ({name:?}).pattern {pat:?} is not a valid regex: {e}",
                    name = bl.name,
                    pat = bl.pattern
                ))
            })?;
            if bl.enums.is_empty() {
                return Err(Error::Config(format!(
                    "rules.body_line[{idx}] ({name:?}).enums must have at least one entry — \
                     a body_line rule without an enum check has no failure mode and would \
                     silently never fire",
                    name = bl.name
                )));
            }
            let capture_names: Vec<&str> = re.capture_names().flatten().collect();
            for capture in bl.enums.keys() {
                if !capture_names.contains(&capture.as_str()) {
                    return Err(Error::Config(format!(
                        "rules.body_line[{idx}] ({name:?}).enums.{capture} is not a named \
                         capture in pattern {pat:?}; declared captures: {caps:?}",
                        name = bl.name,
                        pat = bl.pattern,
                        caps = capture_names
                    )));
                }
            }
            for (capture, allowed) in &bl.enums {
                if allowed.is_empty() {
                    return Err(Error::Config(format!(
                        "rules.body_line[{idx}] ({name:?}).enums.{capture} is empty; \
                         an empty allowed set rejects every captured value",
                        name = bl.name
                    )));
                }
                let mut seen_values = std::collections::BTreeSet::new();
                if let Some(dup) = allowed.iter().find(|v| !seen_values.insert(v.as_str())) {
                    return Err(Error::Config(format!(
                        "rules.body_line[{idx}] ({name:?}).enums.{capture} lists {dup:?} \
                         more than once — drop the duplicate",
                        name = bl.name
                    )));
                }
            }
            self.validate_kinds(
                &format!("rules.body_line[{idx}] ({name:?})", name = bl.name),
                &bl.kinds,
            )?;
        }

        // Annotation patterns: compile, key ∈ named captures, kinds
        // valid, names unique. Same "no silent runtime skips" discipline
        // as everywhere else — a typo in `key` or `kinds`
        // would otherwise silently extract zero markers forever.
        let mut annotation_names: std::collections::BTreeSet<&str> =
            std::collections::BTreeSet::new();
        for (idx, ann) in self.annotations.iter().enumerate() {
            if ann.name.trim().is_empty() {
                return Err(Error::Config(format!(
                    "annotations[{idx}].name must be a non-empty string"
                )));
            }
            if !annotation_names.insert(ann.name.as_str()) {
                return Err(Error::Config(format!(
                    "annotations[{idx}].name {:?} is declared more than once; \
                     names must be unique so CLI filters and JSON output stay deterministic",
                    ann.name
                )));
            }
            let re = regex::Regex::new(&ann.pattern).map_err(|e| {
                Error::Config(format!(
                    "annotations[{idx}] ({name:?}).pattern {pat:?} is not a valid regex: {e}",
                    name = ann.name,
                    pat = ann.pattern
                ))
            })?;
            let capture_names: Vec<&str> = re.capture_names().flatten().collect();
            if !capture_names.iter().any(|n| *n == ann.key) {
                return Err(Error::Config(format!(
                    "annotations[{idx}] ({name:?}).key {key:?} is not a named capture in \
                     pattern {pat:?}; declared captures: {caps:?}",
                    name = ann.name,
                    key = ann.key,
                    pat = ann.pattern,
                    caps = capture_names
                )));
            }
            self.validate_kinds(
                &format!("annotations[{idx}] ({name:?})", name = ann.name),
                &ann.kinds,
            )?;
        }

        // The graph has no notion of "no extensions"; an empty list
        // would silently turn off body-link extraction altogether.
        if self.parser.extensions.is_empty() {
            return Err(Error::Config(
                "parser.extensions must list at least one extension; \
                 omit the key to accept the default [\".md\"]"
                    .to_string(),
            ));
        }
        for (idx, ext) in self.parser.extensions.iter().enumerate() {
            if !ext.starts_with('.') || ext.len() < 2 {
                return Err(Error::Config(format!(
                    "parser.extensions[{idx}] {ext:?} must start with '.' and have at least one character after it"
                )));
            }
        }
        Ok(())
    }

    /// `[trust]` and `[similarity]` weights: every component finite and
    /// non-negative with a positive sum (so the renormalised composite
    /// has a defined denominator), per-kind `trust.overrides` non-empty
    /// and non-overlapping, and `similarity.default_limit >= 1`.
    fn validate_scoring(&self) -> Result<()> {
        let w = &self.trust.weights;
        for (name, value) in [
            ("status", w.status),
            ("freshness", w.freshness),
            ("drift", w.drift),
            ("backlinks", w.backlinks),
        ] {
            if value < 0.0 || !value.is_finite() {
                return Err(Error::Config(format!(
                    "trust.weights.{name} must be a finite non-negative number; got {value}"
                )));
            }
        }
        let w_sum = w.status + w.freshness + w.drift + w.backlinks;
        if !w_sum.is_finite() || w_sum <= 0.0 {
            return Err(Error::Config(
                "trust.weights must have at least one positive component \
                 and a finite sum"
                    .into(),
            ));
        }
        // Trust weight overrides: reject duplicate kinds, validate
        // weight values. Mirrors the schema.overrides overlap
        // detection — first-match lookup means a kind in two
        // overrides would silently ignore the second block.
        let mut trust_kind_origin: BTreeMap<&str, usize> = BTreeMap::new();
        for (idx, ov) in self.trust.overrides.iter().enumerate() {
            let ctx = format!("trust.overrides[{idx}]");
            if ov.kinds.is_empty() {
                return Err(Error::Config(format!("{ctx}.kinds must not be empty")));
            }
            self.validate_kinds(&ctx, &ov.kinds)?;

            for kind in &ov.kinds {
                if let Some(prev) = trust_kind_origin.insert(kind.as_str(), idx) {
                    return Err(Error::Config(format!(
                        "trust.overrides[{idx}] declares kind {kind:?} which is \
                         already covered by trust.overrides[{prev}]"
                    )));
                }
            }

            let tw = &ov.weights;
            for (name, value) in [
                ("status", tw.status),
                ("freshness", tw.freshness),
                ("drift", tw.drift),
                ("backlinks", tw.backlinks),
            ] {
                if value < 0.0 || !value.is_finite() {
                    return Err(Error::Config(format!(
                        "{ctx}.weights.{name} must be a finite non-negative number; got {value}"
                    )));
                }
            }
            let tw_sum = tw.status + tw.freshness + tw.drift + tw.backlinks;
            if !tw_sum.is_finite() || tw_sum <= 0.0 {
                return Err(Error::Config(format!(
                    "{ctx}.weights must have at least one positive component \
                     and a finite sum"
                )));
            }
        }

        // Similarity: same shape as trust.
        let sw = &self.similarity.weights;
        for (name, value) in [
            ("title", sw.title),
            ("tags", sw.tags),
            ("kind", sw.kind),
            ("directory", sw.directory),
            ("linked", sw.linked),
        ] {
            if value < 0.0 || !value.is_finite() {
                return Err(Error::Config(format!(
                    "similarity.weights.{name} must be a finite non-negative number; got {value}"
                )));
            }
        }
        let sw_sum = sw.title + sw.tags + sw.kind + sw.directory + sw.linked;
        if !sw_sum.is_finite() || sw_sum <= 0.0 {
            return Err(Error::Config(
                "similarity.weights must have at least one positive component \
                 and a finite sum"
                    .into(),
            ));
        }
        if self.similarity.default_limit == 0 {
            return Err(Error::Config(
                "similarity.default_limit must be ≥ 1 — `0` would never return any candidate"
                    .into(),
            ));
        }

        // Search: the third ranking surface. Same finite / non-negative /
        // positive-sum discipline as trust and similarity — an all-zero
        // weight set would rank every match identically, collapsing the
        // ordering the command exists to provide.
        let rw = &self.search.weights;
        for (name, value) in [
            ("id_exact", rw.id_exact),
            ("id_partial", rw.id_partial),
            ("title_exact", rw.title_exact),
            ("title_partial", rw.title_partial),
            ("tag", rw.tag),
        ] {
            if value < 0.0 || !value.is_finite() {
                return Err(Error::Config(format!(
                    "search.weights.{name} must be a finite non-negative number; got {value}"
                )));
            }
        }
        let rw_sum = rw.id_exact + rw.id_partial + rw.title_exact + rw.title_partial + rw.tag;
        if !rw_sum.is_finite() || rw_sum <= 0.0 {
            return Err(Error::Config(
                "search.weights must have at least one positive component and a finite sum".into(),
            ));
        }
        Ok(())
    }

    /// Relation-filtered rules: `detection.git_drift_relations` and
    /// `rules.acyclic_relations`. Both are validated unconditionally —
    /// each must be non-empty, duplicate-free, and reference only known
    /// relations. `git_drift_relations` is checked even when
    /// `git_drift_threshold = None`: a typo must not load clean only to
    /// error when drift is later switched on (see the body comment). An
    /// empty list or an unknown relation would otherwise silently match
    /// nothing.
    fn validate_relations(&self) -> Result<()> {
        // `git_drift_relations` is structurally meaningful independent of
        // whether drift is currently enabled — a typo'd relation must not
        // load clean under `git_drift_threshold = None` only to error when
        // drift is later switched on. Validate it unconditionally, the same
        // way `acyclic_relations` is validated below. (The runtime
        // threshold gate that disables measurement when drift is off is a
        // separate, load-bearing concern and stays untouched.)
        if self.detection.git_drift_relations.is_empty() {
            return Err(Error::Config(
                "detection.git_drift_relations must list at least one relation".to_string(),
            ));
        }
        let mut seen_drift = std::collections::BTreeSet::new();
        if let Some(dup) = self
            .detection
            .git_drift_relations
            .iter()
            .find(|r| !seen_drift.insert(r.as_str()))
        {
            return Err(Error::Config(format!(
                "detection.git_drift_relations lists {dup:?} more than once — drop the duplicate"
            )));
        }
        let known = self.known_relations();
        for (idx, rel) in self.detection.git_drift_relations.iter().enumerate() {
            if !known.contains(rel) {
                let known_sorted: Vec<&str> = known.iter().map(String::as_str).collect();
                return Err(Error::Config(format!(
                    "detection.git_drift_relations[{idx}] {rel:?} is not a known relation; \
                     declare it via [[parser.link_patterns]] or pick one of {known_sorted:?}"
                )));
            }
        }

        if self.rules.acyclic_relations.is_empty() {
            return Err(Error::Config(
                "rules.acyclic_relations must list at least one relation".to_string(),
            ));
        }
        // A duplicated relation would run the cycle check twice and
        // report the same ring as two identical violations, inflating
        // the count — reject the typo like every other list.
        let mut seen_relations = std::collections::BTreeSet::new();
        if let Some(dup) = self
            .rules
            .acyclic_relations
            .iter()
            .find(|r| !seen_relations.insert(r.as_str()))
        {
            return Err(Error::Config(format!(
                "rules.acyclic_relations lists {dup:?} more than once — drop the duplicate"
            )));
        }
        let known = self.known_relations();
        for (idx, rel) in self.rules.acyclic_relations.iter().enumerate() {
            if !known.contains(rel) {
                let known_sorted: Vec<&str> = known.iter().map(String::as_str).collect();
                return Err(Error::Config(format!(
                    "rules.acyclic_relations[{idx}] {rel:?} is not a known relation; \
                     declare it via [[parser.link_patterns]] or pick one of {known_sorted:?}"
                )));
            }
        }
        Ok(())
    }

    /// The two diff-aware lock families (`rules.body_immutable`,
    /// `rules.frontmatter_immutable`). Both surface as `<family>/<name>`
    /// rule_ids and route through one validator so a future third
    /// family lands once.
    fn validate_immutability(&self) -> Result<()> {
        validate_immutable_blocks(
            self,
            "rules.body_immutable",
            self.rules.body_immutable.iter().map(|b| ImmutableBlock {
                name: &b.name,
                fields: None,
                kinds: &b.kinds,
            }),
        )?;
        validate_immutable_blocks(
            self,
            "rules.frontmatter_immutable",
            self.rules
                .frontmatter_immutable
                .iter()
                .map(|b| ImmutableBlock {
                    name: &b.name,
                    fields: Some(&b.fields),
                    kinds: &b.kinds,
                }),
        )?;
        Ok(())
    }

    /// Per-kind `[[schema.overrides]]`: reject kinds covered by more
    /// than one block (first-match lookup would silently drop the
    /// later one), validate each block's `kinds` filter and field
    /// declarations, and reject a `cross_field` entry that duplicates a
    /// global one (it would double-report on every matching node).
    fn validate_schema_overrides(&self) -> Result<()> {
        let mut kind_origin: BTreeMap<&str, usize> = BTreeMap::new();
        for (idx, ov) in self.schema.overrides.iter().enumerate() {
            for kind in &ov.kinds {
                if let Some(prev) = kind_origin.insert(kind.as_str(), idx) {
                    return Err(Error::Config(format!(
                        "schema.overrides[{idx}] declares kind {kind:?} which is \
                         already covered by schema.overrides[{prev}]; only the \
                         earlier block would take effect — merge them or \
                         re-partition the kind sets"
                    )));
                }
            }
        }

        for (idx, ov) in self.schema.overrides.iter().enumerate() {
            // `schema_override_for`'s membership lookup makes an empty
            // list silently inert — the cardinal rule: every filter
            // must be non-empty (mirrors trust.overrides).
            if ov.kinds.is_empty() {
                return Err(Error::Config(format!(
                    "schema.overrides[{idx}].kinds must not be empty"
                )));
            }
            let ctx = format!("schema.overrides[{idx}] (kinds={:?})", ov.kinds);
            self.validate_kinds(&ctx, &ov.kinds)?;
            self.validate_block(&ctx, &ov.required, &ov.types, &ov.enums, &ov.cross_field)?;
            for cf in &ov.cross_field {
                if self
                    .schema
                    .cross_field
                    .iter()
                    .any(|g| g.when == cf.when && g.require == cf.require)
                {
                    return Err(Error::Config(format!(
                        "{ctx}: cross_field {{ when={:?}, require={:?} }} \
                         is already declared in [schema].cross_field — \
                         remove the override copy or change its predicate",
                        cf.when, cf.require
                    )));
                }
            }
        }
        Ok(())
    }

    /// Validate one rule block's `kinds` filter against
    /// [`KindsConfig::allowed`]. Centralised so every rule family
    /// rejects an out-of-vocabulary kind with the same message shape.
    fn validate_kinds(&self, ctx: &str, kinds: &[String]) -> Result<()> {
        for kind in kinds {
            if !self.kinds.allowed.iter().any(|k| k == kind) {
                return Err(Error::Config(format!(
                    "{ctx}.kinds contains {kind:?} which is not in \
                     kinds.allowed; add the kind or drop the filter"
                )));
            }
        }
        Ok(())
    }

    /// Validate `schema.require_explicit`. Each entry must be an
    /// inferrable built-in whose authored-vs-inferred distinction is
    /// meaningful — `id` / `title` / `kind` / `status`. `orphan_ok` is
    /// rejected (a bool is structurally always present, so requiring it
    /// to be "authored" would force boilerplate on every document); a
    /// non-inferred field is rejected toward `schema.required` (presence
    /// of an authored field is that rule's job). Duplicates are rejected
    /// like the `required` dup guard — an accepted value always drives
    /// the conditionally-registered `explicit_field` rule.
    fn validate_require_explicit(&self) -> Result<()> {
        let mut seen: Vec<&str> = Vec::new();
        for field in &self.schema.require_explicit {
            if field == "orphan_ok" {
                return Err(Error::Config(
                    "schema.require_explicit lists \"orphan_ok\": a bool is structurally \
                     always present, so \"authored vs omitted\" is not a meaningful \
                     distinction for it — requiring it would force `orphan_ok: false` \
                     boilerplate on every document. Remove it."
                        .into(),
                ));
            }
            if !INFERRED_FRONTMATTER_FIELDS.contains(&field.as_str()) {
                return Err(Error::Config(format!(
                    "schema.require_explicit lists {field:?}, which the parser does not \
                     infer — require_explicit only forbids falling back on an *inferred* \
                     built-in (id / title / kind / status). To require an authored \
                     project field, use schema.required instead."
                )));
            }
            if seen.contains(&field.as_str()) {
                return Err(Error::Config(format!(
                    "schema.require_explicit lists {field:?} more than once"
                )));
            }
            seen.push(field.as_str());
        }
        Ok(())
    }

    /// Validate one schema block (the global [schema] or one override).
    /// Extracted so both share the same rules.
    fn validate_block(
        &self,
        ctx: &str,
        required: &[String],
        types: &BTreeMap<String, FieldType>,
        enums: &BTreeMap<String, Vec<String>>,
        cross_field: &[CrossFieldSpec],
    ) -> Result<()> {
        // A duplicated `required` entry is a config typo that leaks:
        // `export schema` emits the list as a JSON-Schema `required`
        // array, whose elements the draft 2020-12 metaschema requires
        // to be unique (`uniqueItems`) — a strict downstream validator
        // would reject the exported contract.
        let mut seen_required = std::collections::BTreeSet::new();
        if let Some(dup) = required.iter().find(|v| !seen_required.insert(v.as_str())) {
            return Err(Error::Config(format!(
                "{ctx}: required lists {dup:?} more than once — drop the duplicate"
            )));
        }

        // A required collection-valued built-in (`tags`, `supersedes`,
        // `implements`, `related`, `covers`) is self-inconsistent: the
        // only value `scaffold` / `migrate` can default it to is `[]`,
        // which `required_field` treats as missing — so every tool-written
        // document would fail this very rule. Same reasoning as the
        // empty-enum guard below. Reject it; these relations are
        // intrinsically optional (a document with no successor / no tags
        // is normal), and a required-presence intent has no tool-writable
        // satisfying value.
        if let Some(field) = required.iter().find(|f| is_collection_builtin(f)) {
            return Err(Error::Config(format!(
                "{ctx}: required lists {field:?}, a collection-valued built-in — \
                 scaffold/migrate can only default it to [], which the rule treats as \
                 missing, so any tool-written document would fail this rule. Collections \
                 are intrinsically optional; drop it from required."
            )));
        }

        // `path` (and any reserved structural field) names the node's
        // filesystem path, not frontmatter — it is queryable via
        // `query nodes --where` / `--fields` and validated with
        // `[[rules.naming]]`, never a schema rule. Declaring it in
        // required / types / enums would give `path` a second meaning the
        // runtime's `read_field_as_string` can't honor (it always returns
        // the filesystem path), so a `field_enum` would red every
        // document against the path. Reject the declaration at load.
        if let Some(field) = required
            .iter()
            .map(String::as_str)
            .chain(types.keys().map(String::as_str))
            .chain(enums.keys().map(String::as_str))
            .find(|f| is_reserved_structural_field(f))
        {
            return Err(Error::Config(format!(
                "{ctx}: {field:?} is a reserved structural field (the node's filesystem \
                 path), not frontmatter — drop it from required / types / enums and \
                 validate the path with [[rules.naming]] instead"
            )));
        }

        // The parser resolves every inferred built-in for every
        // document, so a `required` entry naming one is satisfied by
        // construction and could never fire — the same
        // accepted-but-inert class as `types` on built-ins below.
        // "No silent runtime skips": reject it with the resolving
        // fallback as remediation.
        if let Some(field) = required
            .iter()
            .find(|f| INFERRED_FRONTMATTER_FIELDS.contains(&f.as_str()))
        {
            return Err(Error::Config(format!(
                "{ctx}: required lists {field:?}, a field the parser always resolves \
                 (id/kind from identity rules, status from statuses.initial, title from \
                 the H1 or filename stem, orphan_ok defaults to false) — the rule could \
                 never fire. Drop it; presence is guaranteed by construction, and value \
                 constraints belong in types / enums"
            )));
        }

        // `field_type` reads only project-specific keys on `Node::attrs`
        // — built-in fields are strongly typed by the parser itself, so
        // a `types` entry naming one is accepted-but-inert forever.
        // "No silent runtime skips": reject it at load with the reason.
        for field in types.keys() {
            if is_builtin_node_field(field) {
                return Err(Error::Config(format!(
                    "{ctx}: types.{field} — built-in fields are typed by the parser and \
                     never reach the field_type rule; drop the entry (types constrains \
                     project-specific frontmatter keys only)"
                )));
            }
        }

        for (field, allowed) in enums {
            if is_collection_builtin(field) {
                return Err(Error::Config(format!(
                    "{ctx}: enums.{field} — collection-valued built-in \
                     fields cannot have a scalar enum constraint"
                )));
            }
            // An empty value list is an unsatisfiable constraint — no
            // value can be a member of `[]`. Worse, it breaks
            // self-consistency: scaffold defaults a required enum field
            // to its first value, which here falls through to `""`, and
            // the tool's own `check` then flags the document it just
            // wrote. Reject at load, symmetric with the body_line guard.
            if allowed.is_empty() {
                return Err(Error::Config(format!(
                    "{ctx}: enums.{field} must list at least one value — an empty enum is \
                     unsatisfiable, and scaffold would write a document that fails this \
                     very rule"
                )));
            }
            let global = match field.as_str() {
                "status" => Some((&self.statuses.allowed, "statuses.allowed")),
                "kind" => Some((&self.kinds.allowed, "kinds.allowed")),
                _ => None,
            };
            if let Some((global, key)) = global {
                for value in allowed {
                    if !global.contains(value) {
                        return Err(Error::Config(format!(
                            "{ctx}: enums.{field} contains {value:?} \
                             which is not in {key}"
                        )));
                    }
                }
            }

            // A duplicated enum value is a config typo that leaks into
            // `export schema` / `export enums` verbatim (the JSON-Schema
            // `enum` slot) — same rationale as the vocabulary lists.
            let mut seen_values = std::collections::BTreeSet::new();
            if let Some(dup) = allowed.iter().find(|v| !seen_values.insert(v.as_str())) {
                return Err(Error::Config(format!(
                    "{ctx}: enums.{field} lists {dup:?} more than once — drop the duplicate"
                )));
            }
        }

        // Two identical entries in one list would double-report every
        // matching node (the cross-block twin of this guard lives in
        // validate_schema_overrides).
        let mut seen_cross = std::collections::BTreeSet::new();
        if let Some(dup) = cross_field
            .iter()
            .find(|cf| !seen_cross.insert((cf.when.as_str(), cf.require.as_str())))
        {
            return Err(Error::Config(format!(
                "{ctx}: cross_field (when={:?}, require={:?}) is declared more than once — drop the duplicate",
                dup.when, dup.require
            )));
        }

        Ok(())
    }
}
