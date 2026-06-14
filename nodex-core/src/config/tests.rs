use super::types::{default_acyclic_relations, default_unresolved_policy};
use super::*;
use crate::error::Error;
use std::collections::BTreeMap;

#[test]
fn parse_when_accepts_simple_equality() {
    let p = parse_when("status=superseded").unwrap();
    assert_eq!(
        p,
        WhenPredicate::Equals {
            field: "status".into(),
            value: "superseded".into()
        }
    );
}

#[test]
fn parse_when_trims_whitespace() {
    let p = parse_when("  status  =  superseded  ").unwrap();
    assert_eq!(
        p,
        WhenPredicate::Equals {
            field: "status".into(),
            value: "superseded".into()
        }
    );
}

#[test]
fn parse_when_rejects_double_equals() {
    assert!(parse_when("status==foo").is_err());
}

#[test]
fn parse_when_rejects_empty_sides() {
    assert!(parse_when("=foo").is_err());
    assert!(parse_when("field=").is_err());
    assert!(parse_when("").is_err());
}

#[test]
fn parse_when_rejects_triple_equals() {
    assert!(parse_when("a=b=c").is_err());
}

#[test]
fn parse_when_accepts_in_syntax() {
    let p = parse_when("status in {active,archived}").unwrap();
    assert_eq!(
        p,
        WhenPredicate::In {
            field: "status".into(),
            values: vec!["active".into(), "archived".into()],
        }
    );
}

#[test]
fn parse_when_accepts_in_with_whitespace() {
    let p = parse_when("status in { active , archived }").unwrap();
    assert_eq!(
        p,
        WhenPredicate::In {
            field: "status".into(),
            values: vec!["active".into(), "archived".into()],
        }
    );
}

#[test]
fn parse_when_rejects_in_empty_values() {
    assert!(parse_when("status in {}").is_err());
}

#[test]
fn parse_when_rejects_in_with_empty_element() {
    assert!(parse_when("status in {active,,archived}").is_err());
}

#[test]
fn parse_when_accepts_exists() {
    let p = parse_when("owner exists").unwrap();
    assert_eq!(
        p,
        WhenPredicate::Exists {
            field: "owner".into(),
        }
    );
}

#[test]
fn parse_when_accepts_not_exists() {
    let p = parse_when("reviewed not_exists").unwrap();
    assert_eq!(
        p,
        WhenPredicate::NotExists {
            field: "reviewed".into(),
        }
    );
}

#[test]
fn parse_when_rejects_exists_empty_field() {
    assert!(parse_when(" exists").is_err());
}

fn override_with(kind: &str, mut ov: SchemaOverride) -> Config {
    ov.kinds = vec![kind.into()];
    let mut kinds = KindsConfig::default();
    if !kinds.allowed.iter().any(|k| k == kind) {
        kinds.allowed.push(kind.into());
    }
    Config {
        kinds,
        schema: SchemaConfig {
            overrides: vec![ov],
            ..Default::default()
        },
        ..Config::default()
    }
}

#[test]
fn validate_rejects_enum_on_collection_field() {
    let config = override_with(
        "adr",
        SchemaOverride {
            kinds: vec![],
            required: vec![],
            types: BTreeMap::new(),
            enums: [("tags".to_string(), vec!["foo".into()])]
                .into_iter()
                .collect(),
            cross_field: vec![],
        },
    );
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("collection-valued"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_enum_value_outside_global_allowed() {
    // `statuses.allowed` must cover the four lifecycle target
    // statuses (superseded / archived / deprecated / abandoned);
    // include them so this test isolates the "enum value outside
    // allowed" check rather than tripping the lifecycle-coverage
    // check first. The override targets `adr`, which must also
    // be in `kinds.allowed` or `validate_kinds` would intercept
    // ahead of the enum check.
    let config = Config {
        kinds: KindsConfig {
            allowed: vec![
                "generic".into(),
                "guide".into(),
                "readme".into(),
                "adr".into(),
            ],
        },
        statuses: StatusesConfig {
            allowed: vec![
                "active".into(),
                "superseded".into(),
                "archived".into(),
                "deprecated".into(),
                "abandoned".into(),
            ],
            terminal: vec![],
            initial: None,
        },
        schema: SchemaConfig {
            overrides: vec![SchemaOverride {
                kinds: vec!["adr".into()],
                required: vec![],
                types: BTreeMap::new(),
                enums: [("status".to_string(), vec!["active".into(), "bogus".into()])]
                    .into_iter()
                    .collect(),
                cross_field: vec![],
            }],
            ..Default::default()
        },
        ..Config::default()
    };
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("bogus"));
            assert!(msg.contains("statuses.allowed"));
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_cross_field_unknown_field() {
    let config = override_with(
        "adr",
        SchemaOverride {
            kinds: vec![],
            required: vec![],
            types: BTreeMap::new(),
            enums: BTreeMap::new(),
            cross_field: vec![CrossFieldSpec {
                when: "statuz=superseded".into(),
                require: "superseded_by".into(),
            }],
        },
    );
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("unknown field"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

/// Build a config with a single global `cross_field` predicate.
fn global_cross_field(when: &str, require: &str) -> Config {
    Config {
        schema: SchemaConfig {
            cross_field: vec![CrossFieldSpec {
                when: when.into(),
                require: require.into(),
            }],
            ..Default::default()
        },
        ..Config::default()
    }
}

fn assert_value_rejected(config: Config) {
    match config.validate().unwrap_err() {
        Error::Config(msg) => assert!(
            msg.contains("never hold") || msg.contains("never fire"),
            "expected a never-fire rejection, got: {msg}"
        ),
        other => panic!("expected Config error, got {other:?}"),
    }
}

#[test]
fn validate_rejects_cross_field_status_value_outside_vocabulary() {
    // `status=draftt` names a status no node can hold (the FieldEnumRule
    // pins status to statuses.allowed), so the predicate could never
    // fire — a silent skip, rejected at load.
    assert_value_rejected(global_cross_field("status=draftt", "superseded_by"));
}

#[test]
fn validate_rejects_cross_field_kind_value_outside_vocabulary() {
    // `kind=adrr` names a kind outside kinds.allowed.
    assert_value_rejected(global_cross_field("kind=adrr", "superseded_by"));
}

#[test]
fn validate_rejects_cross_field_malformed_date_value() {
    // `created` is a date built-in the runtime compares in canonical
    // `%Y-%m-%d` form; `2026-1-1` (unpadded) can never match.
    assert_value_rejected(global_cross_field("created=2026-1-1", "superseded_by"));
}

#[test]
fn validate_rejects_cross_field_in_value_outside_vocabulary() {
    // The same guard applies per-value to an `in {…}` predicate.
    assert_value_rejected(global_cross_field(
        "status in {active,draftt}",
        "superseded_by",
    ));
}

#[test]
fn validate_accepts_cross_field_status_value_in_vocabulary() {
    let config = Config {
        statuses: StatusesConfig {
            allowed: vec!["active".into(), "superseded".into()],
            terminal: vec!["superseded".into()],
            initial: None,
        },
        ..global_cross_field("status=superseded", "superseded_by")
    };
    config
        .validate()
        .expect("a status value in the vocabulary is valid");
}

#[test]
fn validate_accepts_cross_field_canonical_date_value() {
    global_cross_field("created=2026-01-01", "superseded_by")
        .validate()
        .expect("a canonical %Y-%m-%d date predicate is valid");
}

#[test]
fn validate_accepts_cross_field_freeform_attr_sentinel_value() {
    // A free-form attr (declared via `required`, no type/enum)
    // legitimately carries arbitrary sentinel values, so a `when`
    // comparing it to any string must NOT be rejected — constraining it
    // would itself be a false positive.
    let config = Config {
        schema: SchemaConfig {
            required: vec!["priority".into()],
            cross_field: vec![CrossFieldSpec {
                when: "priority=high".into(),
                require: "superseded_by".into(),
            }],
            ..Default::default()
        },
        ..Config::default()
    };
    config
        .validate()
        .expect("a free-form attr accepts any sentinel value");
}

#[test]
fn validate_error_includes_override_context() {
    let config = Config {
        kinds: KindsConfig {
            allowed: vec![
                "generic".into(),
                "guide".into(),
                "readme".into(),
                "adr".into(),
            ],
        },
        schema: SchemaConfig {
            overrides: vec![SchemaOverride {
                kinds: vec!["adr".into(), "guide".into()],
                required: vec![],
                types: BTreeMap::new(),
                enums: [("tags".to_string(), vec!["x".into()])]
                    .into_iter()
                    .collect(),
                cross_field: vec![],
            }],
            ..Default::default()
        },
        ..Config::default()
    };
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("overrides[0]"));
            assert!(msg.contains("\"adr\""));
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_empty_schema() {
    Config::default().validate().unwrap();
}

#[test]
fn validate_accepts_narrow_status_set_without_lifecycle_targets() {
    // A project that only models draft/active/archived must load
    // cleanly — lifecycle vocabulary is not forced onto projects
    // that never run those actions. Self-consistency is enforced at
    // the lifecycle write seam, not by forcing every target status
    // into `statuses.allowed`.
    let config = Config {
        statuses: StatusesConfig {
            allowed: vec!["draft".into(), "active".into(), "archived".into()],
            terminal: vec!["archived".into()],
            initial: Some("draft".into()),
        },
        ..Config::default()
    };
    config.validate().expect("a narrow status set is valid");
}

#[test]
fn validate_rejects_effectful_duplicates_in_remaining_lists() {
    // The guard policy: a duplicate is rejected wherever it changes
    // an output — an exported enum array, a doubled violation, a
    // doubled extracted edge.
    for (toml, needle) in [
        (
            // enum value list → exported enum slot
            "[scope]\ninclude = [\"**/*.md\"]\n[schema]\nenums = { tier = [\"gold\", \"gold\"] }\n",
            "enums.tier",
        ),
        (
            // identical cross_field twice → doubled violations
            "[scope]\ninclude = [\"**/*.md\"]\n[schema]\nrequired = [\"owner\"]\n\
                 cross_field = [{ when = \"owner exists\", require = \"owner\" }, { when = \"owner exists\", require = \"owner\" }]\n",
            "cross_field",
        ),
        (
            // identical naming block twice → doubled violations
            "[scope]\ninclude = [\"**/*.md\"]\n\
                 [[rules.naming]]\nglob = \"docs/**\"\npattern = \"^[a-z-]+$\"\n\
                 [[rules.naming]]\nglob = \"docs/**\"\npattern = \"^[a-z-]+$\"\n",
            "rules.naming",
        ),
        (
            // identical link_pattern twice → doubled edges
            "[scope]\ninclude = [\"**/*.md\"]\n\
                 [[parser.link_patterns]]\npattern = \"@ref\\\\(([^)]+)\\\\)\"\nrelation = \"refs\"\n\
                 [[parser.link_patterns]]\npattern = \"@ref\\\\(([^)]+)\\\\)\"\nrelation = \"refs\"\n",
            "link_patterns",
        ),
        (
            // body_line enum value list → doubled allowed-set entry
            "[scope]\ninclude = [\"**/*.md\"]\n\
                 [[rules.body_line]]\nname = \"lvl\"\npattern = \"level: (?P<lvl>\\\\w+)\"\n\
                 enums = { lvl = [\"info\", \"info\"] }\n",
            "enums.lvl",
        ),
    ] {
        let config: Config = toml::from_str(toml).expect("parses");
        let err = config.validate().expect_err("duplicate must be refused");
        assert!(err.to_string().contains(needle), "{needle}: {err}");
    }

    // Near-duplicates differing in any field are legitimate — the
    // guards reject exact identity only, never over-block.
    for toml in [
        // same glob, different pattern
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [[rules.naming]]\nglob = \"docs/**\"\npattern = \"^[a-z-]+$\"\n\
             [[rules.naming]]\nglob = \"docs/**\"\npattern = \"\\\\.md$\"\n",
        // same pattern, different relation
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [[parser.link_patterns]]\npattern = \"@ref\\\\(([^)]+)\\\\)\"\nrelation = \"refs\"\n\
             [[parser.link_patterns]]\npattern = \"@see\\\\(([^)]+)\\\\)\"\nrelation = \"refs\"\n",
        // same when, different require
        "[scope]\ninclude = [\"**/*.md\"]\n[schema]\nrequired = [\"owner\", \"reviewed\"]\n\
             cross_field = [{ when = \"owner exists\", require = \"reviewed\" }, { when = \"owner exists\", require = \"owner\" }]\n",
    ] {
        let config: Config = toml::from_str(toml).expect("parses");
        config
            .validate()
            .expect("near-duplicate must stay accepted");
    }
}

#[test]
fn validate_rejects_an_unsatisfiable_kind_enum() {
    // A merged enums.kind view that omits a kind it governs makes
    // that kind unsatisfiable: field_enum rejects every document of
    // the kind forever (and the exported schema would disagree).
    // Override self-contradiction:
    let config: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [kinds]\nallowed = [\"generic\", \"adr\", \"guide\"]\n\
             [[schema.overrides]]\nkinds = [\"adr\"]\nenums = { kind = [\"guide\"] }\n",
    )
    .expect("parses");
    let err = config.validate().expect_err("unsatisfiable adr refused");
    assert!(err.to_string().contains("\"adr\""), "{err}");

    // Global enums.kind that strands a residual kind:
    let config: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [kinds]\nallowed = [\"generic\", \"adr\"]\n\
             [schema]\nenums = { kind = [\"adr\"] }\n",
    )
    .expect("parses");
    let err = config.validate().expect_err("stranded generic refused");
    assert!(err.to_string().contains("\"generic\""), "{err}");

    // Consistent (each kind admitted by its merged view) stays accepted.
    let config: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [kinds]\nallowed = [\"generic\", \"adr\"]\n\
             [schema]\nenums = { kind = [\"generic\", \"adr\"] }\n\
             [[schema.overrides]]\nkinds = [\"adr\"]\nenums = { kind = [\"adr\"] }\n",
    )
    .expect("parses");
    config.validate().expect("satisfiable views accepted");
}

#[test]
fn validate_rejects_enum_on_a_boolean_field() {
    // A boolean already permits exactly true/false, so an enum on it
    // is a meaningless constraint — and ill-defined in the export
    // (string enum values vs a boolean JSON type). Rejected for the
    // orphan_ok built-in and any `type = "bool"` field alike.
    for toml in [
        "[scope]\ninclude = [\"**/*.md\"]\n[schema]\nenums = { orphan_ok = [\"true\", \"false\"] }\n",
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [schema]\ntypes = { flag = \"bool\" }\nenums = { flag = [\"true\"] }\n",
    ] {
        let config: Config = toml::from_str(toml).expect("parses");
        let err = config.validate().expect_err("enum on bool refused");
        assert!(
            err.to_string().contains("boolean field already permits"),
            "{err}"
        );
    }

    // Split across global type + override enum: each block's local
    // view is clean, but the MERGED per-kind view is the boolean
    // conflict — caught by `validate_merged_field_enums`.
    let split: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [kinds]\nallowed = [\"generic\", \"adr\"]\n\
             [schema]\ntypes = { flag = \"bool\" }\n\
             [[schema.overrides]]\nkinds = [\"adr\"]\nenums = { flag = [\"yes\"] }\n",
    )
    .expect("parses");
    let err = split.validate().expect_err("split bool+enum refused");
    assert!(
        err.to_string().contains("boolean field already permits"),
        "{err}"
    );

    // Split type-mismatch: global integer type + override non-numeric
    // enum — the merged view rejects the value that cannot parse.
    let split_ty: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [kinds]\nallowed = [\"generic\", \"adr\"]\n\
             [schema]\ntypes = { pri = \"integer\" }\n\
             [[schema.overrides]]\nkinds = [\"adr\"]\nenums = { pri = [\"notnum\"] }\n",
    )
    .expect("parses");
    let err = split_ty
        .validate()
        .expect_err("split type-mismatch refused");
    assert!(
        err.to_string().contains("pri") && err.to_string().contains("\"notnum\""),
        "{err}"
    );
}

#[test]
fn validate_rejects_a_required_collection_builtin() {
    // A required collection built-in can only be defaulted to `[]` by
    // scaffold/migrate, which `required_field` treats as missing — so
    // every tool-written doc would fail it. Rejected at load.
    for field in ["tags", "supersedes", "implements", "related", "covers"] {
        let toml = format!(
            "[scope]\ninclude = [\"**/*.md\"]\n[schema]\nrequired = [\"owner\", \"{field}\"]\n"
        );
        let config: Config = toml::from_str(&toml).expect("parses");
        let err = config.validate().expect_err("required collection refused");
        assert!(
            err.to_string().contains(field) && err.to_string().contains("collection"),
            "{field}: {err}"
        );
    }
}

#[test]
fn validate_rejects_duplicate_required_entries() {
    // A duplicated `required` entry leaks into `export schema` as a
    // JSON-Schema `required` array with non-unique elements — the
    // draft 2020-12 metaschema demands `uniqueItems`. Refused in the
    // global block and in overrides (shared validate_block).
    let config: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [schema]\nrequired = [\"owner\", \"created\", \"owner\"]\n",
    )
    .expect("parses");
    let err = config.validate().expect_err("dup required refused");
    assert!(
        err.to_string().contains("required") && err.to_string().contains("\"owner\""),
        "{err}"
    );

    let config: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [kinds]\nallowed = [\"generic\", \"adr\"]\n\
             [[schema.overrides]]\nkinds = [\"adr\"]\n\
             required = [\"decision_date\", \"decision_date\"]\n",
    )
    .expect("parses");
    let err = config.validate().expect_err("override dup refused");
    assert!(err.to_string().contains("decision_date"), "{err}");
}

#[test]
fn validate_rejects_duplicate_acyclic_relations() {
    // A duplicated relation would run the cycle check twice and
    // report one ring as two identical violations.
    let config: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [rules]\nacyclic_relations = [\"implements\", \"implements\"]\n",
    )
    .expect("parses");
    let err = config.validate().expect_err("dup relation refused");
    assert!(
        err.to_string().contains("acyclic_relations") && err.to_string().contains("implements"),
        "{err}"
    );
}

#[test]
fn validate_rejects_duplicate_vocabulary_entries() {
    // A duplicated vocabulary entry is a config typo that leaks into
    // `export schema` / `export enums` as a JSON-Schema `enum` with
    // non-unique elements (spec-invalid under draft 2020-12) — an
    // output-changing duplicate, which the guard policy rejects.
    for (toml, needle) in [
        (
            "[scope]\ninclude = [\"**/*.md\"]\n[kinds]\nallowed = [\"generic\", \"generic\"]\n",
            "kinds.allowed",
        ),
        (
            "[scope]\ninclude = [\"**/*.md\"]\n\
                 [statuses]\nallowed = [\"active\", \"active\"]\nterminal = []\n",
            "statuses.allowed",
        ),
        (
            "[scope]\ninclude = [\"**/*.md\"]\n\
                 [statuses]\nallowed = [\"active\", \"archived\"]\n\
                 terminal = [\"archived\", \"archived\"]\n",
            "statuses.terminal",
        ),
    ] {
        let config: Config = toml::from_str(toml).expect("parses");
        let err = config.validate().expect_err("duplicate must be refused");
        // Both the list name AND the duplicated value are named, so
        // the operator can fix the typo without hunting.
        assert!(err.to_string().contains(needle), "{needle}: {err}");
        assert!(
            err.to_string().contains("more than once"),
            "{needle}: {err}"
        );
    }
}

#[test]
fn validate_rejects_an_empty_enum_value_list() {
    // An empty enum is unsatisfiable and breaks self-consistency:
    // scaffold would default the required field to `""` and the
    // tool's own check would then reject the document. Refused at
    // load, in the global block and overrides, symmetric with the
    // body_line empty-enums guard.
    let mut config: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [schema]\nrequired = [\"sev\"]\nenums = { sev = [] }\n",
    )
    .expect("parses");
    let err = config.validate().expect_err("empty enum must be refused");
    assert!(err.to_string().contains("enums.sev"), "{err}");

    // A non-empty enum stays valid.
    config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [schema]\nenums = { sev = [\"low\"] }\n",
    )
    .expect("parses");
    config.validate().expect("a populated enum is valid");
}

#[test]
fn validate_rejects_types_entry_on_a_builtin_field() {
    // `field_type` reads only project-specific `attrs` keys —
    // built-ins are parser-typed, so a `types` entry naming one is
    // accepted-but-inert forever. "No silent runtime skips": refuse
    // at load, for scalar and collection built-ins alike, in the
    // global block and in overrides.
    for field in ["owner", "created", "status", "tags"] {
        let mut config = Config::default();
        config
            .schema
            .types
            .insert(field.to_string(), FieldType::String);
        let err = config.validate().expect_err("builtin must be refused");
        assert!(err.to_string().contains(&format!("types.{field}")), "{err}");
    }
    // A project-specific key stays legal.
    let mut config = Config::default();
    config
        .schema
        .types
        .insert("priority".to_string(), FieldType::Integer);
    config.validate().expect("project key is valid");
}

#[test]
fn validate_rejects_required_entry_for_an_inferred_builtin() {
    // The parser resolves these five for every document, so a
    // `required` entry naming one could never fire —
    // accepted-but-inert, the same class as `types` on built-ins.
    // Refused in the global block and in overrides (shared
    // validate_block), with the resolving fallback as remediation.
    for field in ["id", "title", "kind", "status", "orphan_ok"] {
        let mut config = Config::default();
        config.schema.required = vec![field.to_string()];
        let err = config.validate().expect_err("inferred must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("{field:?}")) && msg.contains("the parser always resolves"),
            "{field}: {msg}"
        );

        let mut config = Config::default();
        config.schema.overrides.push(SchemaOverride {
            kinds: vec!["generic".into()],
            required: vec![field.to_string()],
            ..Default::default()
        });
        let err = config
            .validate()
            .expect_err("inferred refused in an override too");
        assert!(
            err.to_string().contains("schema.overrides[0]"),
            "{field}: {err}"
        );
    }
    // Non-inferred built-ins and declared attrs stay legal.
    let mut config = Config::default();
    config.schema.required = vec!["created".into(), "owner".into()];
    config.validate().expect("authored fields are valid");
}

#[test]
fn validate_rejects_cross_field_require_naming_an_inferred_builtin() {
    // `is_field_missing` is always false for id/title/kind/status
    // post-inference, so a `require` naming one is the same
    // accepted-but-inert class the `required` guard rejects.
    // `when = "status=…"` predicates stay legal — they read values,
    // not presence.
    for field in ["id", "title", "kind", "status"] {
        let mut config = Config::default();
        config.schema.cross_field.push(CrossFieldSpec {
            when: "owner exists".into(),
            require: field.to_string(),
        });
        let err = config.validate().expect_err("inert require refused");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("{field:?}")) && msg.contains("could never fire"),
            "{field}: {msg}"
        );
    }
    // `orphan_ok` is exempt — its pass-once-declared semantics are
    // the documented predicate contract.
    let mut config = Config::default();
    config.schema.cross_field.push(CrossFieldSpec {
        when: "owner exists".into(),
        require: "orphan_ok".into(),
    });
    config.validate().expect("require orphan_ok stays legal");
    // …and a `when` keyed on status stays legal too.
    let mut config = Config::default();
    config.schema.cross_field.push(CrossFieldSpec {
        when: "status=superseded".into(),
        require: "superseded_by".into(),
    });
    config.validate().expect("status-keyed when stays legal");
}

#[test]
fn validate_rejects_schema_override_with_empty_kinds() {
    // `schema_override_for`'s membership lookup makes an empty
    // list silently inert — the cardinal rule, mirroring
    // trust.overrides.
    let mut config = Config::default();
    config.schema.overrides.push(SchemaOverride {
        kinds: vec![],
        required: vec!["created".into()],
        ..Default::default()
    });
    let err = config.validate().expect_err("empty kinds refused");
    assert!(
        err.to_string()
            .contains("schema.overrides[0].kinds must not be empty"),
        "{err}"
    );
}

#[test]
fn validate_compiles_scope_globs_at_load() {
    // Load-accept implies scan-success: the loader runs the
    // scanner's own glob compile over include AND the effective
    // excludes (user excludes + the derived output-dir
    // self-exclusion), so a pattern that loads can never fail the
    // first scan.
    let mut config = Config::default();
    config.scope.include = vec!["docs/[**.md".into()];
    let err = config.validate().expect_err("bad include glob refused");
    assert!(
        err.to_string().contains("scope.include")
            && err.to_string().contains("docs/[**.md")
            && err.to_string().contains("not a valid glob"),
        "{err}"
    );

    let mut config = Config::default();
    config.scope.exclude = vec!["[bad".into()];
    let err = config.validate().expect_err("bad exclude glob refused");
    assert!(
        err.to_string().contains("scope.exclude") && err.to_string().contains("[bad"),
        "{err}"
    );

    // The derived self-exclusion glob ("<output.dir>/**") is part of
    // the compiled surface — glob metacharacters in output.dir fail
    // the same load-time compile.
    let mut config = Config::default();
    config.output.dir = "_in[dex".into();
    let err = config
        .validate()
        .expect_err("uncompilable self-exclusion refused");
    assert!(
        err.to_string().contains("_in[dex"),
        "the derived glob names the offending dir: {err}"
    );
}

#[test]
fn validate_guards_prune_dirs() {
    // A path separator means it is a path, not a basename.
    let mut config = Config::default();
    config.scope.prune_dirs = vec!["build/cache".into()];
    let err = config
        .validate()
        .expect_err("path-separated prune dir refused");
    assert!(
        err.to_string().contains("prune_dirs") && err.to_string().contains("path separator"),
        "{err}"
    );

    // A glob metacharacter — pruning is a plain segment match.
    let mut config = Config::default();
    config.scope.prune_dirs = vec!["tmp*".into()];
    let err = config.validate().expect_err("globbed prune dir refused");
    assert!(err.to_string().contains("glob metacharacter"), "{err}");

    // Duplicates are a typo.
    let mut config = Config::default();
    config.scope.prune_dirs = vec!["x".into(), "x".into()];
    let err = config.validate().expect_err("duplicate prune dir refused");
    assert!(err.to_string().contains("more than once"), "{err}");

    // An empty list is legal — it prunes nothing.
    let mut config = Config::default();
    config.scope.prune_dirs = vec![];
    config
        .validate()
        .expect("empty prune_dirs prunes nothing and is valid");
}

#[test]
fn validate_rejects_empty_output_dir() {
    // Artefacts would land in the project root and the GRAPH.md
    // self-exclusion would not engage — GRAPH.md would be re-scanned
    // as a user document.
    let mut config = Config::default();
    config.output.dir = String::new();
    let err = config.validate().expect_err("empty output.dir refused");
    assert!(
        err.to_string().contains("output.dir is empty") && err.to_string().contains("_index"),
        "{err}"
    );
}

#[test]
fn schema_override_required_is_optional_in_toml() {
    // Every override sub-block is opt-in — an override that only
    // narrows enums needs no `required` at all (omitted = adds
    // nothing on top of the global set).
    let config: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [kinds]\nallowed = [\"generic\", \"adr\"]\n\
             [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
             [[schema.overrides]]\nkinds = [\"adr\"]\n\
             enums = { priority = [\"low\", \"high\"] }\n",
    )
    .expect("an override without `required` must deserialize");
    assert!(config.schema.overrides[0].required.is_empty());
    assert_eq!(config.required_for("adr"), config.schema.required);
    config.validate().expect("and validate");
}

#[test]
fn required_for_unions_global_and_override() {
    // An override *adds* per-kind required fields and never silently
    // drops a global one — symmetric with types_for / enums_for /
    // cross_field_for and the documented "merge on top of the
    // globals" contract.
    let mut config = Config::default();
    config.schema.required = vec!["owner".into(), "created".into()];
    config.schema.overrides.push(SchemaOverride {
        kinds: vec!["adr".into()],
        required: vec!["decision_date".into()],
        types: BTreeMap::new(),
        enums: BTreeMap::new(),
        cross_field: Vec::new(),
    });

    let req = config.required_for("adr");
    for f in ["owner", "created", "decision_date"] {
        assert!(req.iter().any(|r| r == f), "{f} required for adr: {req:?}");
    }

    // Re-listing a global field in the override does not double-count.
    config.schema.overrides[0].required = vec!["owner".into(), "decision_date".into()];
    let req = config.required_for("adr");
    assert_eq!(req.iter().filter(|r| *r == "owner").count(), 1, "{req:?}");

    // A kind without an override gets exactly the global set.
    assert_eq!(config.required_for("generic"), config.schema.required);
}

#[test]
fn allowed_statuses_for_uses_override_enum_else_global_allowed() {
    // The single source of truth a lifecycle write checks its target
    // against: the kind's narrowing `status` enum when declared, else
    // the global allowed set.
    let config = Config {
        kinds: KindsConfig {
            allowed: vec!["generic".into(), "adr".into()],
        },
        statuses: StatusesConfig {
            allowed: vec!["active".into(), "archived".into(), "superseded".into()],
            terminal: vec!["archived".into(), "superseded".into()],
            initial: Some("active".into()),
        },
        schema: SchemaConfig {
            overrides: vec![SchemaOverride {
                kinds: vec!["adr".into()],
                required: vec![],
                types: BTreeMap::new(),
                enums: [(
                    "status".to_string(),
                    vec!["active".into(), "superseded".into()],
                )]
                .into_iter()
                .collect(),
                cross_field: vec![],
            }],
            ..Default::default()
        },
        ..Config::default()
    };
    config.validate().expect("valid");
    assert_eq!(
        config.allowed_statuses_for("adr"),
        vec!["active".to_string(), "superseded".to_string()]
    );
    assert_eq!(
        config.allowed_statuses_for("generic"),
        vec![
            "active".to_string(),
            "archived".to_string(),
            "superseded".to_string()
        ]
    );
}

#[test]
fn validate_requires_explicit_initial_when_default_excluded_by_status_enum() {
    // No `statuses.initial`, so the implicit default is the first
    // allowed status ("draft"). The status enum excludes "draft", so a
    // scaffolded doc would fail the config's own `field_enum`. Refuse
    // at load rather than silently reading a default out of enum order.
    let config = Config {
        kinds: KindsConfig {
            allowed: vec!["generic".into()],
        },
        statuses: StatusesConfig {
            allowed: vec![
                "draft".into(),
                "active".into(),
                "superseded".into(),
                "archived".into(),
                "deprecated".into(),
                "abandoned".into(),
            ],
            terminal: vec![
                "superseded".into(),
                "archived".into(),
                "deprecated".into(),
                "abandoned".into(),
            ],
            initial: None,
        },
        schema: SchemaConfig {
            enums: [(
                "status".to_string(),
                vec![
                    "active".into(),
                    "superseded".into(),
                    "archived".into(),
                    "deprecated".into(),
                    "abandoned".into(),
                ],
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
        ..Config::default()
    };
    match config.validate().unwrap_err() {
        Error::Config(msg) => {
            assert!(msg.contains("statuses.initial"), "message was: {msg}");
            assert!(msg.contains("draft"), "message was: {msg}");
        }
        other => panic!("expected Config error, got {other:?}"),
    }
}

#[test]
fn initial_status_checked_against_the_merged_per_kind_status_enum() {
    // A global status enum that excludes the initial status but is
    // SHADOWED by an override for every kind must NOT be rejected —
    // every kind's effective (merged) status enum admits the initial.
    let ok: Config = toml::from_str(
            "[scope]\ninclude = [\"**/*.md\"]\n\
             [kinds]\nallowed = [\"generic\", \"adr\"]\n\
             [statuses]\nallowed = [\"draft\", \"active\"]\nterminal = []\ninitial = \"draft\"\n\
             [schema]\nenums = { status = [\"active\"] }\n\
             [[schema.overrides]]\nkinds = [\"generic\"]\nenums = { status = [\"draft\", \"active\"] }\n\
             [[schema.overrides]]\nkinds = [\"adr\"]\nenums = { status = [\"draft\", \"active\"] }\n",
        )
        .expect("parses");
    ok.validate()
        .expect("global status enum shadowed for every kind is fine");

    // But an override whose status enum genuinely excludes the initial
    // makes that kind's scaffold output fail check — rejected, naming
    // the kind.
    let bad: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [kinds]\nallowed = [\"generic\", \"adr\"]\n\
             [statuses]\nallowed = [\"draft\", \"active\"]\nterminal = []\ninitial = \"draft\"\n\
             [[schema.overrides]]\nkinds = [\"adr\"]\nenums = { status = [\"active\"] }\n",
    )
    .expect("parses");
    let err = bad
        .validate()
        .expect_err("override excluding initial rejected");
    assert!(
        err.to_string().contains("draft") && err.to_string().contains("adr"),
        "{err}"
    );
}

#[test]
fn validate_rejects_explicit_initial_excluded_by_status_enum() {
    // An *explicit* `statuses.initial = "draft"` is in `allowed` but not
    // in the status enum — scaffold would write "draft" and then fail the
    // enum rule. The effective-initial guard must catch this too, not
    // only the implicit-default case.
    let config = Config {
        kinds: KindsConfig {
            allowed: vec!["generic".into()],
        },
        statuses: StatusesConfig {
            allowed: vec![
                "draft".into(),
                "active".into(),
                "superseded".into(),
                "archived".into(),
                "deprecated".into(),
                "abandoned".into(),
            ],
            terminal: vec![
                "superseded".into(),
                "archived".into(),
                "deprecated".into(),
                "abandoned".into(),
            ],
            initial: Some("draft".into()),
        },
        schema: SchemaConfig {
            enums: [(
                "status".to_string(),
                vec![
                    "active".into(),
                    "superseded".into(),
                    "archived".into(),
                    "deprecated".into(),
                    "abandoned".into(),
                ],
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
        ..Config::default()
    };
    match config.validate().unwrap_err() {
        Error::Config(msg) => assert!(msg.contains("draft"), "message was: {msg}"),
        other => panic!("expected Config error, got {other:?}"),
    }
}

#[test]
fn validate_accepts_implicit_initial_permitted_by_status_enum() {
    // allowed.first() = "active" is in the status enum, so no explicit
    // `statuses.initial` is required.
    let config = Config {
        kinds: KindsConfig {
            allowed: vec!["generic".into()],
        },
        statuses: StatusesConfig {
            allowed: vec![
                "active".into(),
                "superseded".into(),
                "archived".into(),
                "deprecated".into(),
                "abandoned".into(),
            ],
            terminal: vec![
                "superseded".into(),
                "archived".into(),
                "deprecated".into(),
                "abandoned".into(),
            ],
            initial: None,
        },
        schema: SchemaConfig {
            enums: [(
                "status".to_string(),
                vec![
                    "active".into(),
                    "superseded".into(),
                    "archived".into(),
                    "deprecated".into(),
                    "abandoned".into(),
                ],
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
        ..Config::default()
    };
    assert!(
        config.validate().is_ok(),
        "config should validate: {:?}",
        config.validate()
    );
}

#[test]
fn scope_include_defaults_through_a_partial_scope_table() {
    // A present `[scope]` table that sets only `exclude` /
    // `conditional_exclude` must still take the `**/*.md` include
    // default — the serde footgun is that a bare `#[serde(default)]`
    // resolves the absent field to `[]` (matching nothing).
    for toml in [
        "[scope]\nexclude = [\"zzz/**\"]\n",
        "[scope]\nconditional_exclude = [{ parent_glob = \"a/**\", \
             child_glob = \"**/*\", condition = \"status_terminal\" }]\n",
    ] {
        let config: Config = toml::from_str(toml).expect("parses");
        assert_eq!(
            config.scope.include,
            vec!["**/*.md".to_string()],
            "partial [scope] keeps the include default"
        );
        config.validate().expect("partial scope is valid");
    }

    // An EXPLICIT empty include scans nothing — rejected, not silently
    // accepted into an empty graph.
    let empty: Config = toml::from_str("[scope]\ninclude = []\n").expect("parses");
    let err = empty.validate().expect_err("empty include rejected");
    assert!(err.to_string().contains("scope.include is empty"), "{err}");
}

#[test]
fn validate_rejects_malformed_cross_field_when_without_panicking() {
    // A malformed `cross_field.when` must surface as a graceful
    // CONFIG_ERROR, not panic a `.expect("validated by Config::load")`
    // in `declared_fields_*` (reached by `validate_immutability`
    // before the merged cross_field pass). Global and override.
    for toml in [
        "[scope]\ninclude = [\"**/*.md\"]\n[schema]\n\
             cross_field = [{ when = \"priority\", require = \"owner\" }]\n",
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [kinds]\nallowed = [\"generic\", \"adr\"]\n\
             [[schema.overrides]]\nkinds = [\"adr\"]\n\
             cross_field = [{ when = \"status==active\", require = \"owner\" }]\n",
    ] {
        let config: Config = toml::from_str(toml).expect("parses");
        let err = config.validate().expect_err("malformed when refused");
        assert!(err.to_string().contains("cross_field.when"), "{err}");
    }
}

#[test]
fn validate_rejects_output_dir_escaping_root() {
    // `output.dir` is joined to the project root for every
    // build / report / cache write. A traversal value would
    // silently write artefacts outside the project root. Refuse at load.
    for bad in ["../escape", "/etc/nodex", "docs/../../out"] {
        let config = Config {
            output: OutputConfig {
                dir: bad.to_string(),
            },
            ..Config::default()
        };
        match config.validate() {
            Err(Error::Config(msg)) => assert!(
                msg.contains("output.dir") && msg.contains("escapes"),
                "for {bad:?} got unexpected message: {msg}"
            ),
            other => panic!("value {bad:?} should have been rejected, got {other:?}"),
        }
    }
}

#[test]
fn validate_rejects_kinds_allowed_missing_fallback_kind() {
    // `migrate` / `parse_document` assign the fallback kind
    // ("generic") to any document whose path isn't covered by an
    // `identity.kind_rules` glob. If the user's `kinds.allowed`
    // omits it, that assignment immediately fails FieldEnumRule —
    // the tool writing a document its own config rejects. Refuse
    // at load.
    let config = Config {
        kinds: KindsConfig {
            allowed: vec!["adr".into()],
        },
        ..Config::default()
    };
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("generic"), "message was: {msg}");
            assert!(msg.contains("fallback"), "message was: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_enum_value_failing_its_declared_type() {
    // `types = { priority = "integer" }` paired with
    // `enums = { priority = ["low", "medium", "high"] }` was an
    // accepted config that made `scaffold` emit an immediately-
    // invalid document (first enum value written, then FieldTypeRule
    // flagged it). Both constraints can legally coexist, but each
    // enum value must parse as the declared type.
    let config = Config {
        kinds: KindsConfig {
            allowed: vec![
                "generic".into(),
                "guide".into(),
                "readme".into(),
                "adr".into(),
            ],
        },
        schema: SchemaConfig {
            overrides: vec![SchemaOverride {
                kinds: vec!["adr".into()],
                required: vec![],
                types: [("priority".to_string(), FieldType::Integer)]
                    .into_iter()
                    .collect(),
                enums: [(
                    "priority".to_string(),
                    vec!["low".into(), "medium".into(), "high".into()],
                )]
                .into_iter()
                .collect(),
                cross_field: vec![],
            }],
            ..Default::default()
        },
        ..Config::default()
    };
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("priority"), "message was: {msg}");
            assert!(msg.contains("\"low\""), "message was: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn cross_field_resolves_fields_across_the_merge_boundary() {
    // An override's cross_field may reference a field declared in the
    // GLOBAL [schema]: the merged per-kind view (cross_field_for ∪
    // enums_for) is what check consumes, so block-local validation
    // must not false-reject it.
    let ok: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [kinds]\nallowed = [\"generic\", \"adr\"]\n\
             [schema]\nenums = { severity = [\"low\", \"high\"] }\n\
             [[schema.overrides]]\nkinds = [\"adr\"]\n\
             cross_field = [{ when = \"severity=high\", require = \"owner\" }]\n",
    )
    .expect("parses");
    ok.validate()
        .expect("override cross_field may name a global field");

    // A GLOBAL cross_field applies to every kind, so it may NOT
    // reference a field only some kinds declare — for `generic`
    // (no override) `tier` is undeclared.
    let bad: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [kinds]\nallowed = [\"generic\", \"adr\"]\n\
             [schema]\ncross_field = [{ when = \"tier=gold\", require = \"owner\" }]\n\
             [[schema.overrides]]\nkinds = [\"adr\"]\nenums = { tier = [\"gold\", \"silver\"] }\n",
    )
    .expect("parses");
    let err = bad
        .validate()
        .expect_err("global cf naming an override-only field");
    assert!(
        err.to_string().contains("tier") && err.to_string().contains("generic"),
        "{err}"
    );
}

#[test]
fn global_cross_field_applies_without_override() {
    let config = Config {
        schema: SchemaConfig {
            cross_field: vec![CrossFieldSpec {
                when: "status=superseded".into(),
                require: "superseded_by".into(),
            }],
            ..Default::default()
        },
        ..Config::default()
    };
    config.validate().unwrap();
    let collected = config.cross_field_for("adr");
    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0].require, "superseded_by");
}

#[test]
fn validate_rejects_cross_field_duplicate_across_global_and_override() {
    let config = Config {
        kinds: KindsConfig {
            allowed: vec![
                "generic".into(),
                "guide".into(),
                "readme".into(),
                "adr".into(),
            ],
        },
        schema: SchemaConfig {
            cross_field: vec![CrossFieldSpec {
                when: "status=superseded".into(),
                require: "superseded_by".into(),
            }],
            overrides: vec![SchemaOverride {
                kinds: vec!["adr".into()],
                required: vec![],
                types: BTreeMap::new(),
                enums: BTreeMap::new(),
                cross_field: vec![CrossFieldSpec {
                    when: "status=superseded".into(),
                    require: "superseded_by".into(),
                }],
            }],
            ..Default::default()
        },
        ..Config::default()
    };
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("already declared in [schema].cross_field"));
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_orphan_ok_kind_outside_kinds_allowed() {
    // Listing a kind in `detection.orphan_ok_kinds` that isn't in
    // `kinds.allowed` would let the user think they had exempted
    // a kind from orphan detection while the runtime silently
    // exempts nothing. Refuse at load.
    let config = Config {
        kinds: KindsConfig {
            allowed: vec!["generic".into(), "guide".into(), "readme".into()],
        },
        detection: DetectionConfig {
            orphan_ok_kinds: vec!["skll".into()],
            ..DetectionConfig::default()
        },
        ..Config::default()
    };
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("orphan_ok_kinds"), "message was: {msg}");
            assert!(msg.contains("\"skll\""), "message was: {msg}");
            assert!(msg.contains("kinds.allowed"), "message was: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn is_orphan_ok_kind_matches_configured_entries() {
    let config = Config {
        kinds: KindsConfig {
            allowed: vec!["generic".into(), "skill".into()],
        },
        detection: DetectionConfig {
            orphan_ok_kinds: vec!["skill".into()],
            ..DetectionConfig::default()
        },
        ..Config::default()
    };
    config.validate().unwrap();
    assert!(config.is_orphan_ok_kind("skill"));
    assert!(!config.is_orphan_ok_kind("generic"));
}

#[test]
fn validate_rejects_overlapping_kinds_across_overrides() {
    // Two overrides both targeting `adr` would silently drop the
    // second block's declarations because every lookup helper
    // stops at the first match.
    let config = Config {
        kinds: KindsConfig {
            allowed: vec![
                "generic".into(),
                "guide".into(),
                "readme".into(),
                "adr".into(),
            ],
        },
        schema: SchemaConfig {
            overrides: vec![
                SchemaOverride {
                    kinds: vec!["adr".into()],
                    required: vec!["owner".into()],
                    types: BTreeMap::new(),
                    enums: BTreeMap::new(),
                    cross_field: vec![],
                },
                SchemaOverride {
                    kinds: vec!["adr".into(), "guide".into()],
                    required: vec!["reviewed".into()],
                    types: BTreeMap::new(),
                    enums: BTreeMap::new(),
                    cross_field: vec![],
                },
            ],
            ..Default::default()
        },
        ..Config::default()
    };
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("\"adr\""), "{msg}");
            assert!(msg.contains("overrides[1]"), "{msg}");
            assert!(msg.contains("overrides[0]"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_schema_override_kinds_not_in_allowed() {
    // A typo in `schema.overrides[].kinds` would silently match no
    // document and the override would never fire — the "no silent
    // runtime skips" failure mode. Mirror the kinds-validation
    // every other rule family already runs.
    let config = Config {
        schema: SchemaConfig {
            overrides: vec![SchemaOverride {
                kinds: vec!["adr".into()], // not in default kinds.allowed
                required: vec!["owner".into()],
                types: BTreeMap::new(),
                enums: BTreeMap::new(),
                cross_field: vec![],
            }],
            ..Default::default()
        },
        ..Config::default()
    };
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("schema.overrides[0]"), "{msg}");
            assert!(msg.contains("\"adr\""), "{msg}");
            assert!(msg.contains("kinds.allowed"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_schema_override_kinds_in_allowed() {
    // Positive complement of the rejection test above. Confirms a
    // well-formed override with `kinds` entirely in `kinds.allowed`
    // loads cleanly — guards against an overzealous validator that
    // rejects valid inputs.
    let config = Config {
        kinds: KindsConfig {
            allowed: vec![
                "generic".into(),
                "guide".into(),
                "readme".into(),
                "adr".into(),
            ],
        },
        schema: SchemaConfig {
            overrides: vec![SchemaOverride {
                kinds: vec!["adr".into()],
                required: vec!["owner".into()],
                types: BTreeMap::new(),
                enums: BTreeMap::new(),
                cross_field: vec![],
            }],
            ..Default::default()
        },
        ..Config::default()
    };
    config
        .validate()
        .expect("override with kinds in allowed must load");
}

#[test]
fn validate_rejects_terminal_status_not_in_allowed() {
    // A `statuses.terminal` entry that isn't in `statuses.allowed`
    // is two self-consistency violations at once: any node landing
    // on that status would fail FieldEnumRule, and `lifecycle`
    // transitions targeting it would never terminate. Refuse at
    // load.
    let config = Config {
        statuses: StatusesConfig {
            allowed: vec![
                "active".into(),
                "superseded".into(),
                "archived".into(),
                "deprecated".into(),
                "abandoned".into(),
            ],
            terminal: vec!["frozen".into()], // not in allowed
            initial: None,
        },
        ..Config::default()
    };
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("statuses.terminal"), "{msg}");
            assert!(msg.contains("\"frozen\""), "{msg}");
            assert!(msg.contains("statuses.allowed"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_terminal_subset_of_allowed() {
    // Positive complement. Default config — which carries the
    // canonical lifecycle terminal statuses (`superseded`,
    // `archived`, `deprecated`, `abandoned`) all of which are in
    // `statuses.allowed` — must continue to load. Without this
    // test a regression that swung the subset check to a strict
    // equality could pass silently.
    Config::default()
        .validate()
        .expect("default config's terminal must be a subset of allowed");
}

#[test]
fn validate_rejects_similarity_default_limit_zero() {
    let mut config = Config::default();
    config.similarity.default_limit = 0;
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("similarity.default_limit"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_frontmatter_immutable_unknown_field() {
    // Locking a field name nowhere declared in schema or built-ins
    // is a silent-skip trap — the rule never finds the misspelt
    // field in `field_changes`. `Config::validate` must reject it.
    use crate::config::{FrontmatterImmutableRuleConfig, RulesConfig};
    let config = Config {
        rules: RulesConfig {
            frontmatter_immutable: vec![FrontmatterImmutableRuleConfig {
                name: "lock".into(),
                fields: vec!["superceded_by".into()], // typo

                kinds: vec![],
            }],
            ..Default::default()
        },
        ..Config::default()
    };
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("superceded_by"), "{msg}");
            assert!(msg.contains("frontmatter_immutable"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_frontmatter_immutable_lock_on_id() {
    // `id` passes the field-universe check (it is a built-in field)
    // but cannot be diff-enforced — graph removal can't distinguish a
    // real id change from a scope-out / id-rule re-key, so the lock
    // could only fire as a false positive. Config must reject it
    // rather than accept a lock that silently never fires correctly.
    // `status`, by contrast, IS enforceable (transition stream) and
    // must remain accepted.
    use crate::config::{FrontmatterImmutableRuleConfig, RulesConfig};
    let with_id = Config {
        rules: RulesConfig {
            frontmatter_immutable: vec![FrontmatterImmutableRuleConfig {
                name: "identity".into(),
                fields: vec!["id".into(), "superseded_by".into()],
                kinds: vec![],
            }],
            ..Default::default()
        },
        ..Config::default()
    };
    match with_id.validate().unwrap_err() {
        Error::Config(msg) => {
            assert!(msg.contains("\"id\""), "{msg}");
            assert!(msg.contains("structural"), "{msg}");
        }
        _ => panic!("expected Config error rejecting id lock"),
    }

    // `status` alone must still validate — it is enforceable.
    let with_status = Config {
        rules: RulesConfig {
            frontmatter_immutable: vec![FrontmatterImmutableRuleConfig {
                name: "lifecycle".into(),
                fields: vec!["status".into()],
                kinds: vec![],
            }],
            ..Default::default()
        },
        ..Config::default()
    };
    assert!(
        with_status.validate().is_ok(),
        "a `status` lock must remain accepted"
    );
}

#[test]
fn validate_accepts_frontmatter_immutable_builtin_and_declared_fields() {
    use crate::config::{FrontmatterImmutableRuleConfig, RulesConfig};
    let mut config = Config::default();
    // `superseded_by` is built-in; `decision_date` is declared via types.
    config
        .schema
        .types
        .insert("decision_date".into(), crate::config::FieldType::Date);
    config.rules = RulesConfig {
        frontmatter_immutable: vec![FrontmatterImmutableRuleConfig {
            name: "lock".into(),
            fields: vec!["superseded_by".into(), "decision_date".into()],

            kinds: vec![],
        }],
        ..Default::default()
    };
    config.validate().expect("must accept valid lock list");
}

#[test]
fn validate_rejects_cross_field_require_string_type() {
    // `type = "string"` defaults to `""` which `is_field_missing`
    // treats as missing. A scaffolded / migrated document would
    // immediately fire `cross_field` — exactly the self-consistency
    // gap the validator must close at load time.
    let mut config = Config::default();
    config
        .schema
        .types
        .insert("owner_team".into(), FieldType::String);
    config.schema.cross_field.push(CrossFieldSpec {
        when: "status=active".into(),
        require: "owner_team".into(),
    });
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("owner_team"), "{msg}");
            assert!(msg.contains("string"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_cross_field_require_custom_required_field() {
    // A custom field declared only in `required` (admitted by
    // `ensure_field_known`) but with no `types` / `enums` gets an
    // empty-string scaffold default that `is_field_missing` flags —
    // the same self-consistency gap as a `type = "string"` require.
    // It must be a typed `CONFIG_ERROR` at load, never a panic.
    let mut config = Config::default();
    config.schema.required.push("replacement_note".into());
    config.schema.cross_field.push(CrossFieldSpec {
        when: "status=active".into(),
        require: "replacement_note".into(),
    });
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("replacement_note"), "{msg}");
            assert!(msg.contains("custom field"), "{msg}");
        }
        _ => panic!("expected Config error, not a panic"),
    }
}

#[test]
fn validate_rejects_cross_field_require_collection_builtin() {
    // `tags`, `supersedes`, etc. default to `[]` which the checker
    // treats as missing — same self-consistency gap as the
    // string-type case, on the built-in side.
    let mut config = Config::default();
    config.schema.cross_field.push(CrossFieldSpec {
        when: "status=active".into(),
        require: "tags".into(),
    });
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("tags"), "{msg}");
            assert!(msg.contains("collection"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_cross_field_require_with_enum_or_date_type() {
    // Enum-constrained fields default to a non-empty first value;
    // date-typed fields default to today. Both survive
    // `is_field_missing` so the validator must accept them.
    let mut enum_config = Config::default();
    enum_config.schema.enums.insert(
        "priority".into(),
        vec!["low".into(), "medium".into(), "high".into()],
    );
    enum_config.schema.cross_field.push(CrossFieldSpec {
        when: "status=active".into(),
        require: "priority".into(),
    });
    enum_config
        .validate()
        .expect("enum-constrained require is safe");

    let mut date_config = Config::default();
    date_config
        .schema
        .types
        .insert("decision_date".into(), FieldType::Date);
    date_config.schema.cross_field.push(CrossFieldSpec {
        when: "status=active".into(),
        require: "decision_date".into(),
    });
    date_config.validate().expect("date-typed require is safe");
}

#[test]
fn validate_accepts_cross_field_require_builtin_optional_scalar() {
    // The init template ships exactly this pattern:
    //   when = "status=superseded" require = "superseded_by"
    // It must keep validating.
    // Default statuses include `superseded`, so this is the
    // canonical superseded → superseded_by linkage.
    let mut c = Config::default();
    c.schema.cross_field.push(CrossFieldSpec {
        when: "status=superseded".into(),
        require: "superseded_by".into(),
    });
    c.validate()
        .expect("canonical superseded → superseded_by must validate");
}

#[test]
fn parse_when_error_mentions_quoting_unsupported() {
    let err = parse_when("status==foo").unwrap_err();
    assert!(
        err.contains("expected") && err.contains("got"),
        "error should mention the unexpected input: {err}"
    );
}

// ─── Annotations validation ────────────────────────────────────────

fn annotations_config(blocks: Vec<AnnotationConfig>) -> Config {
    Config {
        annotations: blocks,
        ..Config::default()
    }
}

#[test]
fn validate_accepts_well_formed_annotation_pattern() {
    annotations_config(vec![AnnotationConfig {
        name: "promotes".into(),
        pattern: r"\[PROMOTES:\s*(?P<id>[\w-]+)\]".into(),
        key: "id".into(),

        kinds: vec![],
    }])
    .validate()
    .unwrap();
}

#[test]
fn validate_rejects_duplicate_annotation_name() {
    let err = annotations_config(vec![
        AnnotationConfig {
            name: "x".into(),
            pattern: r"(?P<k>\w+)".into(),
            key: "k".into(),

            kinds: vec![],
        },
        AnnotationConfig {
            name: "x".into(),
            pattern: r"(?P<j>\w+)".into(),
            key: "j".into(),

            kinds: vec![],
        },
    ])
    .validate()
    .unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("declared more than once"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_annotation_pattern_invalid_regex() {
    let err = annotations_config(vec![AnnotationConfig {
        name: "broken".into(),
        pattern: r"(unclosed".into(),
        key: "k".into(),

        kinds: vec![],
    }])
    .validate()
    .unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("is not a valid regex"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_annotation_key_missing_from_pattern() {
    let err = annotations_config(vec![AnnotationConfig {
        name: "typo".into(),
        pattern: r"(?P<id>\w+)".into(),
        // `key` references a capture name that doesn't exist in the
        // pattern — at runtime this would silently extract zero
        // markers, the textbook "no silent runtime skip" violation.
        key: "topic".into(),

        kinds: vec![],
    }])
    .validate()
    .unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("not a named capture"), "{msg}");
            assert!(msg.contains("declared captures"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

// ─── body_line validation ──────────────────────────────────────────

fn body_line_config(blocks: Vec<BodyLineRuleConfig>) -> Config {
    Config {
        rules: RulesConfig {
            body_line: blocks,
            ..Default::default()
        },
        ..Config::default()
    }
}

fn well_formed_body_line() -> BodyLineRuleConfig {
    let mut enums = BTreeMap::new();
    enums.insert("gate".into(), vec!["scope".into(), "design".into()]);
    BodyLineRuleConfig {
        name: "spec-log".into(),
        pattern: r"^- \*\*(?P<gate>[a-z-]+)\*\*".into(),
        enums,

        kinds: vec![],
    }
}

#[test]
fn validate_accepts_well_formed_body_line_block() {
    body_line_config(vec![well_formed_body_line()])
        .validate()
        .unwrap();
}

#[test]
fn validate_rejects_body_line_duplicate_name() {
    let err = body_line_config(vec![well_formed_body_line(), well_formed_body_line()])
        .validate()
        .unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("declared more than once"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_body_line_empty_enums() {
    let mut block = well_formed_body_line();
    block.enums.clear();
    let err = body_line_config(vec![block]).validate().unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("must have at least one entry"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_body_line_enum_capture_missing_from_pattern() {
    let mut block = well_formed_body_line();
    block.enums.clear();
    // `decision` is not a named capture in the pattern.
    block.enums.insert("decision".into(), vec!["accept".into()]);
    let err = body_line_config(vec![block]).validate().unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("is not a named capture"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_body_line_empty_allowed_list() {
    let mut block = well_formed_body_line();
    block.enums.insert("gate".into(), vec![]);
    let err = body_line_config(vec![block]).validate().unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("is empty"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_body_line_unknown_kind() {
    let mut block = well_formed_body_line();
    block.kinds = vec!["spec".into()];
    // Default kinds.allowed has no "spec" — Config::default has only generic/guide/readme.
    let err = body_line_config(vec![block]).validate().unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("not in kinds.allowed"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_body_line_kinds_in_allowed() {
    // Positive complement of `validate_rejects_body_line_unknown_kind`:
    // a block whose `kinds` list is fully covered by `kinds.allowed`
    // must load cleanly. Without this, a regression that
    // accidentally tightened the validator (e.g. requiring kinds
    // be non-empty) could pass with only the negative test green.
    let mut block = well_formed_body_line();
    block.kinds = vec!["guide".into()]; // "guide" is in default kinds.allowed
    body_line_config(vec![block])
        .validate()
        .expect("body_line block with kinds in allowed must load");
}

// ─── Annotations validation ────────────────────────────────────────

#[test]
fn validate_rejects_annotation_unknown_kind() {
    let err = annotations_config(vec![AnnotationConfig {
        name: "promotes".into(),
        pattern: r"(?P<id>\w+)".into(),
        key: "id".into(),
        kinds: vec!["learnng".into()],
    }])
    .validate()
    .unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("not in kinds.allowed"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_annotations_kinds_in_allowed() {
    // Positive complement of `validate_rejects_annotation_unknown_kind`.
    // `validate_accepts_well_formed_annotation_pattern` covers the
    // `kinds: vec![]` (no restriction) shape; this one anchors the
    // *populated* positive path.
    annotations_config(vec![AnnotationConfig {
        name: "promotes".into(),
        pattern: r"(?P<id>[\w-]+)".into(),
        key: "id".into(),
        kinds: vec!["guide".into()],
    }])
    .validate()
    .expect("annotation with kinds in allowed must load");
}

// ─── git_drift_relations validation ────────────────────────────────

#[test]
fn validate_accepts_known_git_drift_relations() {
    let mut config = Config::default();
    config.detection.git_drift_threshold = Some(5);
    config.detection.git_drift_relations =
        vec!["references".into(), "implements".into(), "covers".into()];
    config.validate().expect("built-in relations must validate");
}

#[test]
fn validate_accepts_user_declared_git_drift_relation() {
    // A relation produced by [[parser.link_patterns]] is part of
    // `known_relations()` — git_drift may filter on it.
    let mut config = Config::default();
    config.parser.link_patterns = vec![LinkPattern {
        pattern: r"@import\s+(.+)".into(),
        relation: "imports".into(),
    }];
    config.detection.git_drift_threshold = Some(3);
    config.detection.git_drift_relations = vec!["imports".into()];
    config
        .validate()
        .expect("user-declared relation must validate");
}

#[test]
fn validate_rejects_unknown_git_drift_relation() {
    // A typo would silently match zero edges — `git_drift` would
    // self-report "fine" forever. Refused at load instead.
    let mut config = Config::default();
    config.detection.git_drift_threshold = Some(5);
    config.detection.git_drift_relations = vec!["referenced".into()];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("referenced"), "msg: {msg}");
            assert!(msg.contains("not a known relation"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

// ─── detection.unresolved_policy validation ────────────────────────

fn policy_row(
    name: &str,
    cause: crate::model::UnresolvedCause,
    glob: Option<&str>,
    severity: UnresolvedSeverity,
) -> UnresolvedPolicyRuleConfig {
    UnresolvedPolicyRuleConfig {
        name: name.to_string(),
        cause,
        glob: glob.map(str::to_string),
        severity,
    }
}

#[test]
fn default_config_policy_is_single_excluded_target_info_row() {
    // Default preservation pinned at the value level: a link to a
    // real on-disk file that scope keeps out of the graph is
    // informational; everything else takes the warning fallthrough.
    let policy = &Config::default().detection.unresolved_policy;
    assert_eq!(policy.len(), 1);
    assert_eq!(policy[0].name, "excluded_target");
    assert_eq!(
        policy[0].cause,
        crate::model::UnresolvedCause::ExcludedFromScope
    );
    assert_eq!(policy[0].glob, None);
    assert_eq!(policy[0].severity, UnresolvedSeverity::Info);
    // The serde field default supplies the same value.
    assert_eq!(default_unresolved_policy().len(), 1);
    assert_eq!(default_unresolved_policy()[0].name, "excluded_target");
}

#[test]
fn validate_rejects_empty_unresolved_policy() {
    // Declaring the table replaces the default, so an explicit `[]`
    // configures nothing while looking configured.
    let config: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [detection]\nunresolved_policy = []\n",
    )
    .expect("parses");
    let err = config.validate().expect_err("empty table refused");
    assert!(
        err.to_string().contains("unresolved_policy") && err.to_string().contains("omit the key"),
        "{err}"
    );
}

#[test]
fn validate_rejects_duplicate_policy_name() {
    let mut config = Config::default();
    config.detection.unresolved_policy = vec![
        policy_row(
            "specs",
            crate::model::UnresolvedCause::Missing,
            Some("specs/**"),
            UnresolvedSeverity::Info,
        ),
        policy_row(
            "specs",
            crate::model::UnresolvedCause::Missing,
            Some("docs/**"),
            UnresolvedSeverity::Error,
        ),
    ];
    let err = config.validate().expect_err("duplicate name refused");
    assert!(
        err.to_string().contains("\"specs\"") && err.to_string().contains("more than once"),
        "{err}"
    );
}

#[test]
fn validate_rejects_reserved_policy_name() {
    // Info rows count under `by_category[<name>]` in the same map
    // as the built-in keys — collisions would make one count
    // unreadable as the other.
    for reserved in ["unresolved_edge", "orphan", "stale", "violation_x"] {
        let mut config = Config::default();
        config.detection.unresolved_policy = vec![policy_row(
            reserved,
            crate::model::UnresolvedCause::Missing,
            None,
            UnresolvedSeverity::Info,
        )];
        let err = config
            .validate()
            .expect_err("reserved name must be refused");
        assert!(err.to_string().contains("reserved"), "{reserved:?}: {err}");
    }
}

#[test]
fn validate_rejects_glob_on_pathless_cause() {
    // Ids are not paths, and resolution-time refusals never reach a
    // root-relative resolution — a glob on those causes could never
    // match anything.
    for cause in [
        crate::model::UnresolvedCause::IdNotFound,
        crate::model::UnresolvedCause::EscapesSource,
        crate::model::UnresolvedCause::Absolute,
    ] {
        let mut config = Config::default();
        config.detection.unresolved_policy = vec![policy_row(
            "pathless",
            cause,
            Some("docs/**"),
            UnresolvedSeverity::Info,
        )];
        let err = config
            .validate()
            .expect_err("glob on pathless cause refused");
        assert!(
            err.to_string().contains("no path candidates"),
            "cause {cause:?}: {err}"
        );
    }
}

#[test]
fn validate_rejects_invalid_policy_glob() {
    let mut config = Config::default();
    config.detection.unresolved_policy = vec![policy_row(
        "bad-glob",
        crate::model::UnresolvedCause::Missing,
        Some("docs/[a-"),
        UnresolvedSeverity::Info,
    )];
    let err = config.validate().expect_err("bad glob refused");
    assert!(
        err.to_string().contains("bad-glob") && err.to_string().contains("not a valid glob"),
        "{err}"
    );
}

#[test]
fn validate_rejects_duplicate_cause_glob_pair() {
    // First match wins — an identical later (cause, glob) pair can
    // never fire.
    let mut config = Config::default();
    config.detection.unresolved_policy = vec![
        policy_row(
            "first",
            crate::model::UnresolvedCause::Missing,
            Some("specs/**"),
            UnresolvedSeverity::Info,
        ),
        policy_row(
            "second",
            crate::model::UnresolvedCause::Missing,
            Some("specs/**"),
            UnresolvedSeverity::Error,
        ),
    ];
    let err = config.validate().expect_err("duplicate pair refused");
    assert!(
        err.to_string().contains("\"second\"") && err.to_string().contains("never fire"),
        "{err}"
    );
}

#[test]
fn load_rejects_unknown_cause_string() {
    // The cause vocabulary is the typed enum — an unknown string
    // fails at deserialization, before validate even runs.
    let err = toml::from_str::<Config>(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [[detection.unresolved_policy]]\n\
             name = \"x\"\ncause = \"not_a_cause\"\nseverity = \"info\"\n",
    )
    .expect_err("unknown cause string refused at deserialize");
    assert!(err.to_string().contains("not_a_cause"), "{err}");
}

#[test]
fn load_rejects_unknown_severity_string() {
    let err = toml::from_str::<Config>(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [[detection.unresolved_policy]]\n\
             name = \"x\"\ncause = \"missing\"\nseverity = \"fatal\"\n",
    )
    .expect_err("unknown severity string refused at deserialize");
    assert!(err.to_string().contains("fatal"), "{err}");
}

#[test]
fn declared_policy_table_replaces_the_default() {
    // The acyclic_relations / git_drift_relations replacement
    // discipline: declaring rows drops the default excluded_target
    // row unless re-declared.
    let config: Config = toml::from_str(
        "[scope]\ninclude = [\"**/*.md\"]\n\
             [[detection.unresolved_policy]]\n\
             name = \"ephemeral-specs\"\ncause = \"missing\"\n\
             glob = \"specs/**\"\nseverity = \"info\"\n",
    )
    .expect("parses");
    config.validate().expect("validates");
    assert_eq!(config.detection.unresolved_policy.len(), 1);
    assert_eq!(
        config.detection.unresolved_policy[0].name,
        "ephemeral-specs"
    );
}

// ─── acyclic_relations validation ──────────────────────────────────

#[test]
fn default_acyclic_relations_is_implements_everywhere() {
    // The serde field default and the in-code `Default` impl must
    // supply the same value — a derived impl would produce an
    // empty (load-rejected) list. Regression guard for the
    // explicit `impl Default for RulesConfig`.
    assert_eq!(default_acyclic_relations(), vec!["implements".to_string()]);
    let defaults = RulesConfig::default();
    assert_eq!(defaults.acyclic_relations, vec!["implements".to_string()]);
    assert!(defaults.naming.is_empty());
    assert!(defaults.frontmatter_immutable.is_empty());
    assert!(defaults.body_immutable.is_empty());
    assert!(defaults.body_line.is_empty());
    assert!(defaults.immutable_baseline.is_none());
}

#[test]
fn validate_rejects_empty_acyclic_relations() {
    // The cycle-detection rule is always registered; an empty
    // relation set would silently fire nothing.
    let mut config = Config::default();
    config.rules.acyclic_relations = Vec::new();
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("acyclic_relations"), "msg: {msg}");
            assert!(msg.contains("at least one"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_unknown_acyclic_relation() {
    let mut config = Config::default();
    config.rules.acyclic_relations = vec!["implements".into(), "implments".into()];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("acyclic_relations[1]"), "msg: {msg}");
            assert!(msg.contains("implments"), "msg: {msg}");
            assert!(msg.contains("not a known relation"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_user_declared_acyclic_relation() {
    // A relation produced by [[parser.link_patterns]] is part of
    // `known_relations()` — a project may declare it acyclic.
    let mut config = Config::default();
    config.parser.link_patterns = vec![LinkPattern {
        pattern: r"@depends\s+(.+)".into(),
        relation: "depends_on".into(),
    }];
    config.rules.acyclic_relations = vec!["depends_on".into()];
    config
        .validate()
        .expect("user-declared relation must validate");
}

// ─── [meta] binary-version pin ─────────────────────────────────────
//
// `Config::load` validates the pin's SemVer syntax but does NOT
// enforce it — reads stay available on a mismatched binary. These
// tests exercise the file path end-to-end (load → mutation gate /
// read advisory) so the wiring stays connected.

fn write_config(root: &std::path::Path, body: &str) {
    std::fs::write(root.join("nodex.toml"), body).expect("write nodex.toml");
}

#[test]
fn load_never_enforces_meta_version_so_reads_always_work() {
    // Reading a graph can never corrupt it, so an unsatisfiable pin
    // must NOT block `Config::load` — a mismatched binary still
    // inspects the project. The wildcard and an impossible upper
    // bound both load; enforcement lives in the mutation path.
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "[meta]\nnodex_version = \"*\"\n");
    Config::load(dir.path()).expect("wildcard pin must load");

    write_config(dir.path(), "[meta]\nnodex_version = \"<0.0.1\"\n");
    Config::load(dir.path()).expect("unsatisfiable pin still loads for read-only use");
}

#[test]
fn mutation_load_refuses_binary_outside_meta_version() {
    // The pin's purpose is to stop an incompatible binary from
    // *writing* documents. An upper bound below the current binary
    // surfaces as VERSION_MISMATCH from the mutation loader.
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "[meta]\nnodex_version = \"<0.0.1\"\n");
    let err = crate::load_project_for_mutation(dir.path()).unwrap_err();
    assert_eq!(
        err.code(),
        "VERSION_MISMATCH",
        "mutation on an out-of-pin binary must surface as VERSION_MISMATCH, got {err}"
    );
}

#[test]
fn binary_compat_warning_fires_only_on_real_mismatch() {
    // Read-only commands attach a non-fatal advisory when the binary
    // is outside the pin, and stay silent when it is satisfied.
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "[meta]\nnodex_version = \"<0.0.1\"\n");
    let cfg = Config::load(dir.path()).expect("loads");
    assert!(
        crate::binary_compat_warning(&cfg).is_some(),
        "out-of-pin binary must yield an advisory"
    );

    write_config(dir.path(), "[meta]\nnodex_version = \"*\"\n");
    let cfg = Config::load(dir.path()).expect("loads");
    assert!(
        crate::binary_compat_warning(&cfg).is_none(),
        "satisfied pin must yield no advisory"
    );
}

#[test]
fn load_rejects_unknown_config_key() {
    // Removed or mistyped keys must surface, never be silently
    // absorbed — the config surface honours "no silent runtime
    // skips" via `deny_unknown_fields`.
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "[scope]\ninclude_hidden = true\n");
    let err = Config::load(dir.path()).unwrap_err();
    assert_eq!(
        err.code(),
        "CONFIG_ERROR",
        "unknown config key must surface as CONFIG_ERROR, got {err}"
    );
}

#[test]
fn validate_rejects_meta_version_with_malformed_requirement() {
    // A garbage SemVer requirement is a config defect regardless of
    // which binary reads it — `validate()` rejects it at load time so
    // both read and mutation paths see CONFIG_ERROR.
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "[meta]\nnodex_version = \"not-a-req\"\n");
    let err = Config::load(dir.path()).unwrap_err();
    assert_eq!(
        err.code(),
        "CONFIG_ERROR",
        "malformed requirement must surface as CONFIG_ERROR, got {err}"
    );
}

// ─── [[rules.body_immutable]] validation ───────────────────────────

fn body_immutable_block(name: &str) -> crate::config::BodyImmutableRuleConfig {
    crate::config::BodyImmutableRuleConfig {
        name: name.into(),
        mode: crate::config::BodyImmutableMode::Frozen,
        trigger: crate::config::ImmutableTrigger::Terminal,
        kinds: vec![],
    }
}

#[test]
fn validate_rejects_body_immutable_empty_name() {
    let mut c = Config::default();
    c.rules.body_immutable = vec![body_immutable_block("")];
    let err = c.validate().unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("non-empty"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_body_immutable_duplicate_name() {
    // Two blocks with the same name would emit identical
    // violation rule_ids, making CI dashboards confuse one
    // policy for another. Refused at load.
    let mut c = Config::default();
    c.rules.body_immutable = vec![body_immutable_block("dup"), body_immutable_block("dup")];
    let err = c.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("declared more than once"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_body_immutable_unknown_kind() {
    // A typo in `kinds` would silently match zero
    // documents forever. Same "no silent runtime skips"
    // discipline body_line / annotations apply.
    let mut c = Config::default();
    let mut block = body_immutable_block("policy");
    block.kinds = vec!["adrr".into()]; // typo
    c.rules.body_immutable = vec![block];
    let err = c.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("not in kinds.allowed"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

// ─── [[rules.frontmatter_immutable]] validation ─────────────────────

fn frontmatter_immutable_block(
    name: &str,
    fields: Vec<&str>,
) -> crate::config::FrontmatterImmutableRuleConfig {
    crate::config::FrontmatterImmutableRuleConfig {
        name: name.into(),
        fields: fields.into_iter().map(String::from).collect(),

        kinds: vec![],
    }
}

#[test]
fn validate_rejects_frontmatter_immutable_empty_name() {
    let mut c = Config::default();
    c.rules.frontmatter_immutable = vec![frontmatter_immutable_block("", vec!["kind"])];
    let err = c.validate().unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("non-empty"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_frontmatter_immutable_duplicate_name() {
    let mut c = Config::default();
    c.rules.frontmatter_immutable = vec![
        frontmatter_immutable_block("dup", vec!["kind"]),
        frontmatter_immutable_block("dup", vec!["kind"]),
    ];
    let err = c.validate().unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("declared more than once"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_frontmatter_immutable_empty_fields() {
    // An empty `fields` list locks nothing — refused at load,
    // same reason an empty enums list is refused for body_line.
    let mut c = Config::default();
    c.rules.frontmatter_immutable = vec![frontmatter_immutable_block("empty", vec![])];
    let err = c.validate().unwrap_err();
    match err {
        Error::Config(msg) => assert!(msg.contains("at least one field"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_frontmatter_immutable_kind_scoped() {
    let mut c = Config::default();
    c.kinds.allowed.push("adr".into());
    let mut block = frontmatter_immutable_block("lock", vec!["kind"]);
    block.kinds = vec!["adr".into()];
    c.rules.frontmatter_immutable = vec![block];
    c.validate().expect("well-formed kind filter must load");
}

#[test]
fn validate_rejects_frontmatter_immutable_kinds_not_in_allowed() {
    // Mirror of `validate_rejects_body_line_unknown_kind` and
    // `validate_rejects_annotation_unknown_kind` — the same
    // typo-silently-matches-nothing failure mode lives on the
    // frontmatter_immutable surface too; this negative test anchors
    // the symmetric-guards discipline
    // (`.claude/rules/config-driven.md`).
    let mut c = Config::default();
    // Do *not* add "adr" to kinds.allowed — that's the bug.
    let mut block = frontmatter_immutable_block("lock", vec!["kind"]);
    block.kinds = vec!["adr".into()];
    c.rules.frontmatter_immutable = vec![block];
    let err = c.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("not in kinds.allowed"), "{msg}");
            assert!(msg.contains("frontmatter_immutable"), "{msg}");
            assert!(msg.contains("\"adr\""), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_body_immutable_block_with_allowed_kind() {
    let mut c = Config::default();
    c.kinds.allowed.push("adr".into());
    let mut block = body_immutable_block("policy");
    block.kinds = vec!["adr".into()];
    c.rules.body_immutable = vec![block];
    c.validate().expect("well-formed block must load");
}

#[test]
fn load_accepts_meta_omitted_entirely() {
    // The pin is opt-in. A config with no `[meta]` block must load
    // exactly the same as a config with `nodex_version` unset —
    // this is the recommended default during early development.
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "[scope]\ninclude = [\"**/*.md\"]\n");
    Config::load(dir.path()).expect("absent [meta] must load");
}

#[test]
fn validate_accepts_trust_overrides_with_valid_kinds() {
    let mut config = Config::default();
    config.kinds.allowed.push("adr".into());
    config.trust.overrides = vec![TrustWeightOverride {
        kinds: vec!["adr".into()],
        weights: TrustWeights {
            status: 0.2,
            freshness: 0.2,
            drift: 0.2,
            backlinks: 0.4,
        },
    }];
    config.validate().expect("valid trust override must load");
}

#[test]
fn validate_rejects_trust_override_with_unknown_kind() {
    let mut config = Config::default();
    config.trust.overrides = vec![TrustWeightOverride {
        kinds: vec!["bogus".into()],
        weights: TrustWeights::default(),
    }];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("trust.overrides[0]"), "{msg}");
            assert!(msg.contains("\"bogus\""), "{msg}");
            assert!(msg.contains("kinds.allowed"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_trust_override_duplicate_kind() {
    let mut config = Config::default();
    config.kinds.allowed.push("adr".into());
    config.trust.overrides = vec![
        TrustWeightOverride {
            kinds: vec!["adr".into()],
            weights: TrustWeights::default(),
        },
        TrustWeightOverride {
            kinds: vec!["adr".into()],
            weights: TrustWeights {
                status: 0.1,
                freshness: 0.1,
                drift: 0.1,
                backlinks: 0.7,
            },
        },
    ];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("\"adr\""), "{msg}");
            assert!(msg.contains("overrides[1]"), "{msg}");
            assert!(msg.contains("overrides[0]"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_trust_override_negative_weight() {
    let mut config = Config::default();
    config.kinds.allowed.push("adr".into());
    config.trust.overrides = vec![TrustWeightOverride {
        kinds: vec!["adr".into()],
        weights: TrustWeights {
            status: -0.1,
            freshness: 0.3,
            drift: 0.2,
            backlinks: 0.1,
        },
    }];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("trust.overrides[0]"), "{msg}");
            assert!(msg.contains("status"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_negative_search_weight() {
    let mut config = Config::default();
    config.search.weights.id_exact = -1.0;
    match config.validate().unwrap_err() {
        Error::Config(msg) => {
            assert!(msg.contains("search.weights.id_exact"), "{msg}");
            assert!(msg.contains("finite non-negative"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_all_zero_search_weights() {
    let mut config = Config::default();
    config.search.weights = SearchWeights {
        id_exact: 0.0,
        id_partial: 0.0,
        title_exact: 0.0,
        title_partial: 0.0,
        tag: 0.0,
    };
    match config.validate().unwrap_err() {
        Error::Config(msg) => assert!(msg.contains("search.weights must have"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_default_search_weights() {
    // The default weight set must pass its own validator — a project
    // that writes no `[search]` block gets the working defaults.
    Config::default()
        .validate()
        .expect("default search weights are valid");
}

#[test]
fn validate_rejects_require_explicit_orphan_ok() {
    let mut config = Config::default();
    config.schema.require_explicit = vec!["orphan_ok".into()];
    match config.validate().unwrap_err() {
        Error::Config(msg) => assert!(msg.contains("orphan_ok"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_require_explicit_non_inferred_field() {
    let mut config = Config::default();
    // `created` is authored, not inferred — belongs in schema.required.
    config.schema.require_explicit = vec!["created".into()];
    match config.validate().unwrap_err() {
        Error::Config(msg) => {
            assert!(msg.contains("created"), "{msg}");
            assert!(msg.contains("schema.required"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_require_explicit_duplicate() {
    let mut config = Config::default();
    config.schema.require_explicit = vec!["status".into(), "status".into()];
    match config.validate().unwrap_err() {
        Error::Config(msg) => assert!(msg.contains("more than once"), "{msg}"),
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_require_explicit_inferred_fields() {
    let mut config = Config::default();
    config.schema.require_explicit = vec!["status".into(), "title".into()];
    config
        .validate()
        .expect("id/title/kind/status are valid require_explicit entries");
}

#[test]
fn validate_rejects_trust_override_all_zero_weights() {
    let mut config = Config::default();
    config.kinds.allowed.push("adr".into());
    config.trust.overrides = vec![TrustWeightOverride {
        kinds: vec!["adr".into()],
        weights: TrustWeights {
            status: 0.0,
            freshness: 0.0,
            drift: 0.0,
            backlinks: 0.0,
        },
    }];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("trust.overrides[0]"), "{msg}");
            assert!(msg.contains("at least one positive"), "{msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn trust_weights_for_returns_override_when_matched() {
    let mut config = Config::default();
    config.kinds.allowed.push("adr".into());
    let override_weights = TrustWeights {
        status: 0.1,
        freshness: 0.1,
        drift: 0.1,
        backlinks: 0.7,
    };
    config.trust.overrides = vec![TrustWeightOverride {
        kinds: vec!["adr".into()],
        weights: override_weights,
    }];
    let resolved = config.trust_weights_for("adr");
    assert_eq!(resolved.backlinks, 0.7);
    assert_eq!(resolved.status, 0.1);
    // Unmatched kind falls back to global.
    let fallback = config.trust_weights_for("generic");
    assert_eq!(fallback.status, config.trust.weights.status);
    assert_eq!(fallback.backlinks, config.trust.weights.backlinks);
}

// ─── Phase 3: silent-no-op invariants ──────────────────────────────
//
// Each pair (reject + accept) guards one runtime contract that would
// otherwise let a config load cleanly and produce zero observable
// effect. The validator's job is to refuse precisely the inputs the
// runtime would silently drop.

#[test]
fn validate_rejects_link_pattern_without_capture_group() {
    // `parser::body` extracts edge targets from `caps.get(1)`. A
    // pattern without a `(...)` group silently emits nothing — the
    // user thinks they declared a custom link, the graph has zero
    // edges for it.
    let mut config = Config::default();
    config.parser.link_patterns = vec![LinkPattern {
        pattern: r"@import\s+\S+".into(),
        relation: "imports".into(),
    }];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("parser.link_patterns[0]"), "msg: {msg}");
            assert!(msg.contains("no capture group"), "msg: {msg}");
            assert!(msg.contains("(...)"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_link_pattern_with_capture_group() {
    // The same shape `parser::body` already consumes — at least
    // one explicit capture group so `caps.get(1)` resolves.
    let mut config = Config::default();
    config.parser.link_patterns = vec![LinkPattern {
        pattern: r"@import\s+(\S+)".into(),
        relation: "imports".into(),
    }];
    config
        .validate()
        .expect("link_pattern with one capture group must validate");
}

#[test]
fn validate_rejects_link_pattern_with_covers_relation() {
    // `covers` is the one path-only relation, produced exclusively
    // by the frontmatter `covers:` field. A link pattern naming it
    // would silently switch its targets to path-only resolution —
    // semantics must never attach to a user-chosen relation name,
    // so the shape is refused at load with the remediation.
    let mut config = Config::default();
    config.parser.link_patterns = vec![LinkPattern {
        pattern: r"@covers (\S+)".into(),
        relation: "covers".into(),
    }];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("parser.link_patterns[0]"), "msg: {msg}");
            assert!(msg.contains("\"covers\""), "msg: {msg}");
            assert!(msg.contains("path-only"), "msg: {msg}");
            assert!(msg.contains("frontmatter covers: field"), "msg: {msg}");
            assert!(msg.contains("different relation name"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_link_pattern_with_id_resolved_relations() {
    // `supersedes` / `implements` / `related` resolve strictly by
    // node id — their resolution mode is fixed in code, exactly
    // like the path-only `covers`. A link pattern naming one would
    // silently lose document-reference resolution for its targets,
    // so each is refused at load, naming the frontmatter field as
    // the way to declare the relation plus the remediation.
    for relation in ["supersedes", "implements", "related"] {
        let mut config = Config::default();
        config.parser.link_patterns = vec![LinkPattern {
            pattern: r"@link (\S+)".into(),
            relation: relation.into(),
        }];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("parser.link_patterns[0]"), "msg: {msg}");
                assert!(msg.contains(&format!("{relation:?}")), "msg: {msg}");
                assert!(msg.contains("id-resolved"), "msg: {msg}");
                assert!(
                    msg.contains(&format!("frontmatter {relation}: field")),
                    "msg: {msg}"
                );
                assert!(msg.contains("different relation name"), "msg: {msg}");
            }
            _ => panic!("expected Config error for relation {relation:?}"),
        }
    }
}

#[test]
fn validate_accepts_link_pattern_with_other_relation_names() {
    // `references` (the one built-in whose mode IS document
    // reference — a pattern naming it shifts no semantics) and any
    // user-invented relation name are legal on a pattern.
    for relation in ["cites", "references"] {
        let mut config = Config::default();
        config.parser.link_patterns = vec![LinkPattern {
            pattern: r"@link (\S+)".into(),
            relation: relation.into(),
        }];
        config
            .validate()
            .unwrap_or_else(|e| panic!("relation {relation:?} must validate: {e}"));
    }
}

#[test]
fn validate_rejects_link_pattern_with_multiple_capture_groups() {
    // Multiple capture groups would cause confusion: only the first
    // is used, so having more is a silent misbehavior. Reject explicitly.
    let mut config = Config::default();
    config.parser.link_patterns = vec![LinkPattern {
        pattern: r"@import\s+(\S+)\s+from\s+(\S+)".into(),
        relation: "imports".into(),
    }];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(
                msg.contains("parser.link_patterns[0].pattern"),
                "msg: {msg}"
            );
            assert!(msg.contains("multiple capture groups"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_unknown_conditional_exclude_condition() {
    // `builder::scanner::apply_conditional_excludes` only honours
    // `status_terminal`. A misspelling like `status_terminated`
    // would load cleanly and exclude nothing — a silent no-op
    // rule. Refuse at load with the valid set in the message.
    let mut config = Config::default();
    config.scope.conditional_exclude = vec![ConditionalExclude {
        parent_glob: "specs/*/spec.md".into(),
        child_glob: "**/*".into(),
        condition: "status_terminated".into(),
    }];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("scope.conditional_exclude[0]"), "msg: {msg}");
            assert!(msg.contains("\"status_terminated\""), "msg: {msg}");
            assert!(msg.contains("status_terminal"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_status_terminal_conditional_exclude() {
    let mut config = Config::default();
    config.scope.conditional_exclude = vec![ConditionalExclude {
        parent_glob: "specs/*/spec.md".into(),
        child_glob: "**/*".into(),
        condition: "status_terminal".into(),
    }];
    config
        .validate()
        .expect("status_terminal condition must validate");
}

#[test]
fn validate_rejects_id_rule_kind_not_in_allowed() {
    // `parser::identity::infer_kind` skips id_rules whose `kind`
    // is neither `*` nor the inferred kind. A typo like `"guidde"`
    // would load cleanly and silently never apply. Refuse at load.
    let mut config = Config::default();
    config.identity.id_rules = vec![IdRule {
        kind: "guidde".into(),
        glob: None,
        template: "guide-{stem}".into(),
    }];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("identity.id_rules[0].kind"), "msg: {msg}");
            assert!(msg.contains("\"guidde\""), "msg: {msg}");
            assert!(msg.contains("kinds.allowed"), "msg: {msg}");
            assert!(msg.contains("\"*\""), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_wildcard_kind_in_id_rule() {
    // The any-kind escape hatch documented in `parser::identity`.
    let mut config = Config::default();
    config.identity.id_rules = vec![IdRule {
        kind: "*".into(),
        glob: None,
        template: "{kind}-{stem}".into(),
    }];
    config
        .validate()
        .expect("wildcard kind must validate without being in kinds.allowed");
}

#[test]
fn validate_accepts_id_rule_kind_in_kinds_allowed() {
    // The companion to the wildcard case: an explicit kind that
    // *is* in `kinds.allowed` must load. `"guide"` is one of the
    // default kinds shipped by `default_kinds()`.
    let mut config = Config::default();
    config.identity.id_rules = vec![IdRule {
        kind: "guide".into(),
        glob: None,
        template: "guide-{stem}".into(),
    }];
    config
        .validate()
        .expect("id_rule with kind in kinds.allowed must validate");
}

#[test]
fn validate_rejects_unknown_id_template_placeholder() {
    // `parser::identity::expand_template` only knows about
    // `{kind}`, `{stem}`, `{parent}`, `{path_slug}`. A typo like
    // `{stme}` would otherwise load cleanly and produce a literal
    // `{stme}` substring in every generated id — surfacing the
    // typo at load instead is the symmetric guard.
    let mut config = Config::default();
    config.identity.id_rules = vec![IdRule {
        kind: "*".into(),
        glob: None,
        template: "{kind}-{stme}".into(),
    }];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("identity.id_rules[0].template"), "msg: {msg}");
            assert!(msg.contains("\"stme\""), "msg: {msg}");
            assert!(msg.contains("{kind}"), "msg: {msg}");
            assert!(msg.contains("{path_slug}"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_every_known_id_template_placeholder() {
    // Positive companion to the typo case: every name listed in
    // `ID_TEMPLATE_PLACEHOLDERS` must validate. If a future patch
    // adds a new placeholder to the substitution arms without
    // extending the constant, this test still passes — but its
    // *companion* must be added here too, locking the closed set
    // in sync with the substitution.
    let mut config = Config::default();
    config.identity.id_rules = vec![IdRule {
        kind: "*".into(),
        glob: None,
        template: "{kind}-{stem}-{parent}-{path_slug}".into(),
    }];
    config
        .validate()
        .expect("template referencing every known placeholder must validate");
}

#[test]
fn validate_accepts_id_template_without_any_placeholder() {
    // A literal-only template ("readme-root") is a legitimate
    // use case for path-pinned rules. The placeholder scan must
    // not require at least one `{ident}`.
    let mut config = Config::default();
    config.identity.id_rules = vec![IdRule {
        kind: "*".into(),
        glob: None,
        template: "readme-root".into(),
    }];
    config
        .validate()
        .expect("literal-only template must validate");
}

#[test]
fn validate_accepts_id_template_with_repeated_placeholder() {
    // `{stem}-{stem}` is well-formed: the placeholder regex matches
    // it twice, both names are in `ID_TEMPLATE_PLACEHOLDERS`, and
    // no brace is left over after stripping. The malformed-brace
    // scan must not false-positive on legitimate repetition.
    let mut config = Config::default();
    config.identity.id_rules = vec![IdRule {
        kind: "*".into(),
        glob: None,
        template: "{stem}-{stem}".into(),
    }];
    config
        .validate()
        .expect("repeated well-formed placeholder must validate");
}

#[test]
fn validate_rejects_id_template_with_whitespace_in_braces() {
    // `{ kind }` is not a well-formed placeholder — the regex skips
    // it, the substitution arm in `expand_template` skips it, and
    // the runtime would emit the literal `{ kind }` substring in
    // every generated id. Reject at load with a clear error.
    let mut config = Config::default();
    config.identity.id_rules = vec![IdRule {
        kind: "*".into(),
        glob: None,
        template: "{ kind }-{stem}".into(),
    }];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("identity.id_rules[0].template"), "msg: {msg}");
            assert!(msg.contains("malformed brace"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_id_template_with_unclosed_brace() {
    // `{kind-{stem}` leaves a stray `{` after stripping the
    // well-formed `{stem}` — the runtime would emit `{kind-` into
    // every generated id. Reject at load.
    let mut config = Config::default();
    config.identity.id_rules = vec![IdRule {
        kind: "*".into(),
        glob: None,
        template: "{kind-{stem}".into(),
    }];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("identity.id_rules[0].template"), "msg: {msg}");
            assert!(msg.contains("malformed brace"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_id_template_with_unopened_brace() {
    // `kind}-{stem}` leaves a stray `}` after stripping the
    // well-formed `{stem}` — the runtime would emit `kind}-` into
    // every generated id. Reject at load.
    let mut config = Config::default();
    config.identity.id_rules = vec![IdRule {
        kind: "*".into(),
        glob: None,
        template: "kind}-{stem}".into(),
    }];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("identity.id_rules[0].template"), "msg: {msg}");
            assert!(msg.contains("malformed brace"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_id_template_with_double_braces() {
    // We don't support `{{kind}}` as a literal-brace escape. The
    // inner `{kind}` is well-formed and gets stripped; the outer
    // `{` and `}` are left over and the runtime would emit them
    // literal. Reject at load — keep the substitution model
    // simple, and surface the ambiguity at config load time.
    let mut config = Config::default();
    config.identity.id_rules = vec![IdRule {
        kind: "*".into(),
        glob: None,
        template: "{{kind}}-{stem}".into(),
    }];
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("identity.id_rules[0].template"), "msg: {msg}");
            assert!(msg.contains("malformed brace"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_zero_stale_days() {
    let mut config = Config::default();
    config.detection.stale_days = Some(0);
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("stale_days"), "msg: {msg}");
            assert!(msg.contains("must be > 0 or None"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_rejects_zero_git_drift_threshold() {
    let mut config = Config::default();
    config.detection.git_drift_threshold = Some(0);
    let err = config.validate().unwrap_err();
    match err {
        Error::Config(msg) => {
            assert!(msg.contains("git_drift_threshold"), "msg: {msg}");
            assert!(msg.contains("must be > 0 or None"), "msg: {msg}");
        }
        _ => panic!("expected Config error"),
    }
}

#[test]
fn validate_accepts_none_stale_days() {
    let mut config = Config::default();
    config.detection.stale_days = None;
    assert!(config.validate().is_ok());
}

#[test]
fn validate_accepts_positive_stale_days() {
    let mut config = Config::default();
    config.detection.stale_days = Some(180);
    assert!(config.validate().is_ok());
}
