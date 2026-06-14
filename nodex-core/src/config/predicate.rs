//! The `cross_field.when` predicate language, the field-vocabulary
//! constants/helpers, and the predicate-value validators the schema
//! validators consume.

use std::collections::BTreeMap;

use super::types::*;
use crate::error::{Error, Result};

/// Parsed `cross_field.when` predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhenPredicate {
    /// `<field>=<value>` — match when the given field equals the value exactly.
    Equals { field: String, value: String },
    /// `<field> in {v1,v2,...}` — match when the field's value is one of the listed values.
    In { field: String, values: Vec<String> },
    /// `<field> exists` — match when the field is present (non-empty).
    Exists { field: String },
    /// `<field> not_exists` — match when the field is absent (or empty).
    NotExists { field: String },
}

impl WhenPredicate {
    /// The frontmatter field this predicate tests.
    pub fn field(&self) -> &str {
        match self {
            Self::Equals { field, .. }
            | Self::In { field, .. }
            | Self::Exists { field }
            | Self::NotExists { field } => field,
        }
    }
}

/// Every built-in scalar field on `Node`. Kept here (not on `Node`) so
/// config validation sees the canonical list without pulling in the
/// whole model module. Collections (`tags`, `supersedes`, etc.) are
/// intentionally excluded — they cannot be members of a scalar enum.
pub const BUILTIN_SCALAR_FIELDS: &[&str] = &[
    "id",
    "title",
    "kind",
    "status",
    "created",
    "updated",
    "reviewed",
    "owner",
    "superseded_by",
    "orphan_ok",
];

/// Collection-valued built-in fields. Enum/type constraints on these
/// must be rejected — there is no single scalar value to check.
pub const BUILTIN_COLLECTION_FIELDS: &[&str] =
    &["tags", "supersedes", "implements", "related", "covers"];

/// Field names that address a node's STRUCTURAL identity rather than its
/// frontmatter. They are queryable (`query nodes --where` / `--fields`)
/// but can never be declared in `schema` (no `types` / `enums` /
/// `required` / `cross_field`): `path` is the filesystem path — validate
/// it with `[[rules.naming]]`, not a schema rule. Reserving the name
/// keeps the canonical `path` read (the node's filesystem path) the one
/// meaning everywhere `rules::schema::read_field_as_string` runs, so a
/// project can never give `path` a second, frontmatter meaning that a
/// `field_enum` / `cross_field` rule would read against the filesystem
/// path instead.
pub const RESERVED_STRUCTURAL_FIELDS: &[&str] = &["path"];

/// True when `field` is a reserved structural field name (see
/// [`RESERVED_STRUCTURAL_FIELDS`]).
pub fn is_reserved_structural_field(field: &str) -> bool {
    RESERVED_STRUCTURAL_FIELDS.contains(&field)
}

/// Built-in fields the parser/builder always resolves to a value, so a
/// document may omit them from authored frontmatter: `id` / `kind` /
/// `status` are inferred (id_rules, kind_rules, `statuses.initial`),
/// `title` falls back to the H1 or filename stem, and `orphan_ok`
/// defaults to `false`. A `required` entry naming one of these could
/// therefore never fire — the field is present on every parsed node by
/// construction — so `Config::validate` rejects it at load (the
/// `validate_block` guard), and the exported JSON Schema's `required`
/// arrays carry only authorable fields by construction. The
/// non-inferred fields (dates, `owner`, `superseded_by`, relations)
/// carry no fallback, so requiring them is enforced by both `check`
/// and the schema.
pub const INFERRED_FRONTMATTER_FIELDS: &[&str] = &["id", "title", "kind", "status", "orphan_ok"];

/// True when `field` is one of the built-in `Node` fields of any kind.
pub fn is_builtin_node_field(field: &str) -> bool {
    BUILTIN_SCALAR_FIELDS.contains(&field) || BUILTIN_COLLECTION_FIELDS.contains(&field)
}

/// True when `field` is a built-in collection-valued field.
pub fn is_collection_builtin(field: &str) -> bool {
    BUILTIN_COLLECTION_FIELDS.contains(&field)
}

/// True when the raw frontmatter-style string `value` is a valid
/// member of the declared `FieldType`. Used by `Config::validate` to
/// reject configs that pair a typed field with an enum containing
/// values that can never satisfy the type.
pub(crate) fn value_matches_field_type(value: &str, ty: FieldType) -> bool {
    match ty {
        FieldType::String => true,
        FieldType::Integer => value.parse::<i64>().is_ok(),
        FieldType::Bool => matches!(value, "true" | "false"),
        FieldType::Date => chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok(),
    }
}

/// Reject field names in `cross_field.when` / `cross_field.require`
/// that are not built-in and not declared in the supplied
/// `required` / `types` / `enums`. Callers pass a kind's MERGED view
/// (`validate_merged_cross_fields`), so a predicate may name a field
/// declared in any block that contributes to that kind. Keeps typos from
/// turning into silently-skipped checks.
pub(crate) fn ensure_field_known(
    field: &str,
    required: &[String],
    types: &BTreeMap<String, FieldType>,
    enums: &BTreeMap<String, Vec<String>>,
    ctx: &str,
    slot: &str,
) -> Result<()> {
    if is_reserved_structural_field(field) {
        return Err(Error::Config(format!(
            "{ctx}: {slot} references {field:?}, a reserved structural field (the node's \
             filesystem path) — it is not frontmatter, so a cross_field predicate cannot \
             read it. Validate the path with [[rules.naming]] instead"
        )));
    }
    if is_builtin_node_field(field)
        || required.iter().any(|r| r == field)
        || types.contains_key(field)
        || enums.contains_key(field)
    {
        return Ok(());
    }
    Err(Error::Config(format!(
        "{ctx}: {slot} references unknown field {field:?}; declare it \
         in required / types / enums or use a built-in name"
    )))
}

/// Reject a `cross_field.when` equals/in predicate whose value(s) the
/// field can never hold — so a typo (`status=draftt`, `kind=adrr`,
/// `created=2026-1-1`) is a load error, not a predicate that silently
/// never fires. The accepted-value test mirrors the runtime read
/// (`rules::schema::read_field_as_string`) exactly:
///
/// - `kind` / `status` → the merged per-kind enum view. The caller
///   passes the same backfilled map the runtime's `FieldEnumRule`
///   builds (a per-kind override enum, else the global vocabulary), so
///   a per-kind enum that narrows `status`/`kind` makes a predicate on
///   an excluded value a load error rather than a silently-inert rule;
/// - the date built-ins `created` / `updated` / `reviewed` → a valid
///   `%Y-%m-%d` date, the canonical form the runtime formats before it
///   compares;
/// - `orphan_ok` → `true` / `false`;
/// - an enum-constrained field → an enum member;
/// - a type-constrained field → a value of that type;
/// - a free-form untyped non-enum attr (and the open-string built-ins
///   `id` / `title` / `owner` / `superseded_by`) → unconstrained, since
///   it legitimately carries arbitrary sentinel values a predicate may
///   match. Constraining them would itself be a false positive.
///
/// `exists` / `not_exists` carry no value and are exempt.
pub(crate) fn ensure_predicate_values_match_field(
    predicate: &WhenPredicate,
    types: &BTreeMap<String, FieldType>,
    enums: &BTreeMap<String, Vec<String>>,
    ctx: &str,
) -> Result<()> {
    let (field, values): (&str, &[String]) = match predicate {
        WhenPredicate::Equals { field, value } => (field, std::slice::from_ref(value)),
        WhenPredicate::In { field, values } => (field, values.as_slice()),
        WhenPredicate::Exists { .. } | WhenPredicate::NotExists { .. } => return Ok(()),
    };

    let accepts = |v: &str| -> bool {
        match field {
            // The runtime reads a date built-in in canonical `%Y-%m-%d`
            // form and string-compares, so the predicate value must
            // already be canonical: `2026-1-1` parses but formats back to
            // `2026-01-01`, so it could never match. (A typed-Date *attr*
            // below stays lenient — its stored form is the author's, so a
            // canonical requirement there could false-reject a real match.)
            "created" | "updated" | "reviewed" => chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d")
                .is_ok_and(|d| d.format("%Y-%m-%d").to_string() == v),
            "orphan_ok" => value_matches_field_type(v, FieldType::Bool),
            // `kind` / `status` resolve here too: the caller backfills
            // their vocabularies into `enums` exactly as the runtime does,
            // so they take the enum branch with every other enum field.
            _ => {
                if let Some(allowed) = enums.get(field) {
                    allowed.iter().any(|a| a == v)
                } else if let Some(ty) = types.get(field) {
                    value_matches_field_type(v, *ty)
                } else {
                    true // free-form attr / open-string built-in
                }
            }
        }
    };

    if let Some(bad) = values.iter().find(|v| !accepts(v)) {
        return Err(Error::Config(format!(
            "{ctx}: when predicate compares field {field:?} to {bad:?}, a value the field \
             can never hold — outside its allowed values/type, the rule would never fire. \
             Fix the value, or widen the field's enum/type"
        )));
    }
    Ok(())
}

/// Reject a `cross_field.require` whose field cannot receive a
/// tool-generated default value the same rule then accepts.
///
/// The `is_field_missing` predicate that powers `RequiredFieldRule`
/// and `CrossFieldRule` treats empty strings and empty arrays as
/// missing. So a `cross_field.require` pointing at a `type = "string"`
/// field would fire the moment `scaffold` / `migrate` writes the
/// empty-string default. The valid combinations are enumerated below;
/// anything else would let nodex write a document that fails its own
/// check, violating the "tool-written must pass" invariant.
pub(crate) fn ensure_cross_field_default_satisfiable(
    field: &str,
    types: &BTreeMap<String, FieldType>,
    enums: &BTreeMap<String, Vec<String>>,
    ctx: &str,
) -> Result<()> {
    // Enum-constrained fields default to the first allowed value,
    // which `Config::validate` guarantees is non-empty.
    if enums.contains_key(field) {
        return Ok(());
    }
    // Non-string typed fields default to `today` / `0` / `false` —
    // all non-empty when serialised back.
    if let Some(ty) = types.get(field) {
        return match ty {
            FieldType::Date | FieldType::Integer | FieldType::Bool => Ok(()),
            FieldType::String => Err(Error::Config(format!(
                "{ctx}: cross_field.require {field:?} is declared as `type = \"string\"`; \
                 a scaffolded / migrated document would receive an empty-string default \
                 that this very rule treats as missing. Constrain it with `enums = {{ \
                 {field} = [...] }}` so the default is meaningful, or pick a non-string \
                 type."
            ))),
        };
    }
    // Built-in fields fall into two groups for default-emptiness:
    //   safe   — date fields default to today; Option<String> scalars
    //            (`owner` / `superseded_by`) keep `Some("")` which the
    //            checker does not consider missing; `orphan_ok` is a
    //            bool the checker treats as structurally present.
    //   unsafe — collection-valued built-ins (`supersedes`, `implements`,
    //            `related`, `tags`, `covers`) default to an empty Vec
    //            which `is_field_missing` flags.
    // `id` / `title` / `kind` / `status` never reach this function —
    // `validate_merged_cross_fields` rejects a `require` naming a
    // parser-resolved field before the satisfiability check runs.
    match field {
        "created" | "updated" | "reviewed" | "owner" | "superseded_by" | "orphan_ok" => Ok(()),
        "supersedes" | "implements" | "related" | "tags" | "covers" => Err(Error::Config(format!(
            "{ctx}: cross_field.require {field:?} is a collection-valued built-in; \
             scaffold / migrate default it to `[]` which this very rule treats as \
             missing. Either pick a scalar field, or drop the cross_field constraint."
        ))),
        // A custom field admitted only by `required` (not a built-in, not
        // in `types` / `enums`): `default_for_field` gives it an
        // empty-string default that `is_field_missing` flags, exactly
        // like an unconstrained `type = "string"`, so a scaffolded /
        // migrated document would fail this very rule.
        _ => Err(Error::Config(format!(
            "{ctx}: cross_field.require {field:?} is a custom field with no `types` / `enums` \
             declaration; a scaffolded / migrated document would receive an empty-string default \
             that this very rule treats as missing. Constrain it with `enums = {{ {field} = [...] }}` \
             so the default is meaningful, or declare a non-string `types` entry."
        ))),
    }
}

/// Parse a `cross_field.when` expression.
///
/// Accepted forms:
/// - `<field>=<value>` — equality predicate.
/// - `<field> in {v1,v2,...}` — membership predicate (comma-separated inside braces).
/// - `<field> exists` — presence predicate (field is non-empty).
/// - `<field> not_exists` — absence predicate (field is absent or empty).
///
/// Rejects `==` and any form where the value starts with `=`, so a typo
/// can never silently turn into a predicate that matches nothing. Also
/// rejects empty field names and empty value lists.
pub fn parse_when(raw: &str) -> std::result::Result<WhenPredicate, String> {
    let trimmed = raw.trim();

    // Try keyword-based forms first (whitespace-separated tokens).
    if let Some((field, rest)) = trimmed.split_once(char::is_whitespace) {
        let field = field.trim();
        let rest = rest.trim();
        if rest == "exists" {
            if field.is_empty() {
                return Err("expected non-empty field name before `exists`".to_string());
            }
            return Ok(WhenPredicate::Exists {
                field: field.to_string(),
            });
        }
        if rest == "not_exists" {
            if field.is_empty() {
                return Err("expected non-empty field name before `not_exists`".to_string());
            }
            return Ok(WhenPredicate::NotExists {
                field: field.to_string(),
            });
        }

        // `<field> in {v1,v2,...}` — strip the `in` keyword and parse braced values.
        if rest.starts_with("in ") || rest.starts_with("in\t") || rest == "in" {
            let braced = rest.strip_prefix("in").unwrap().trim();
            return parse_in_predicate(field, braced, raw);
        }
        if let Some(after) = rest.strip_prefix("in{") {
            let braced = format!("{{{after}");
            return parse_in_predicate(field, &braced, raw);
        }
    }

    // Fall through to `<field>=<value>` equality syntax.
    let parts: Vec<&str> = trimmed.splitn(3, '=').collect();
    if parts.len() != 2 {
        return Err(format!(
            "expected `<field>=<value>`, `<field> in {{...}}`, \
             `<field> exists`, or `<field> not_exists` (got {raw:?})"
        ));
    }
    let field = parts[0].trim();
    let value = parts[1].trim();
    if field.is_empty() || value.is_empty() {
        return Err("expected non-empty <field>=<value>".to_string());
    }
    if value.starts_with('=') {
        return Err("value must not start with '=' (use a single '=' separator)".to_string());
    }
    Ok(WhenPredicate::Equals {
        field: field.to_string(),
        value: value.to_string(),
    })
}

/// Helper: parse the `{v1,v2,...}` portion of an `in` predicate.
fn parse_in_predicate(
    field: &str,
    rest: &str,
    raw: &str,
) -> std::result::Result<WhenPredicate, String> {
    if field.is_empty() {
        return Err("expected non-empty field name before `in`".to_string());
    }
    if !rest.starts_with('{') || !rest.ends_with('}') {
        return Err(format!(
            "expected `<field> in {{val1,val2,...}}` with curly braces (got {raw:?})"
        ));
    }
    let inner = &rest[1..rest.len() - 1];
    let values: Vec<String> = inner.split(',').map(|v| v.trim().to_string()).collect();
    if values.is_empty() || values.iter().all(|v| v.is_empty()) {
        return Err(format!(
            "expected at least one non-empty value inside braces (got {raw:?})"
        ));
    }
    for (i, v) in values.iter().enumerate() {
        if v.is_empty() {
            return Err(format!(
                "empty value at position {i} inside braces (got {raw:?})"
            ));
        }
    }
    Ok(WhenPredicate::In {
        field: field.to_string(),
        values,
    })
}
