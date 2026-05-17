# Generating Typed Clients from `nodex export envelope-schema`

`nodex export envelope-schema` emits a draft 2020-12 JSON Schema for
every CLI envelope shape. It is the **canonical contract** between
nodex and any downstream that parses its JSON output — types
generated from it cannot drift from what the CLI actually serialises,
because the schema is derived from the same Rust structs that produce
the JSON.

This guide is the recommended way to consume that schema. Skipping it
and hand-rolling envelope types is supported but loses the contract
gate; nodex's own release process changes envelope shape only in
minor-or-major bumps, so consumers without a regeneration step learn
about drift at runtime.

## The pattern

Three pieces, one direction:

1. **Generate** — pipe `nodex export envelope-schema` into a codegen
   tool that emits typed bindings for your language.
2. **Commit** — check the generated file into version control. This
   makes the contract visible to humans and AI agents reading the
   codebase.
3. **Gate** — re-run the generation in CI. If the diff is non-empty,
   the CI fails with a typed error pointing at the changed shape.
   This catches envelope drift on `nodex` upgrades before runtime.

```
nodex export envelope-schema → schema.json → [codegen] → typed client → CI drift gate
```

## Python (Pydantic via `datamodel-code-generator`)

`datamodel-code-generator` is the de-facto Python codegen for JSON
Schema. Pydantic models give parse-time validation; the resulting
file is fully typed, IDE/AI-friendly, and contains no hand-written
field translations.

### Install (project-local)

```bash
uv add --dev datamodel-code-generator
# or: pip install datamodel-code-generator
```

### Generate

```bash
nodex export envelope-schema > _generated/nodex_envelopes.schema.json
uv run datamodel-codegen \
    --input _generated/nodex_envelopes.schema.json \
    --input-file-type jsonschema \
    --output _generated/nodex_envelopes.py \
    --output-model-type pydantic_v2.BaseModel
```

### Consume

```python
from _generated.nodex_envelopes import QueryAnnotationsEnvelope

proc = subprocess.run(
    ["nodex", "query", "annotations", "--with-frontmatter", "created,tags"],
    capture_output=True, text=True, check=True,
)
envelope = QueryAnnotationsEnvelope.model_validate_json(proc.stdout)
for group in envelope.data.items:
    for entry in group.entries:
        for source in entry.sources:
            # Built-in fields are typed.
            print(source.source_id, source.line)
            # Project-declared frontmatter stays a dict — the schema
            # cannot know per-project keys (e.g. `priority`, `owner`).
            created = source.frontmatter.get("created")
```

### Drift gate (CI step)

```bash
nodex export envelope-schema > _generated/nodex_envelopes.schema.json.new
uv run datamodel-codegen \
    --input _generated/nodex_envelopes.schema.json.new \
    --input-file-type jsonschema \
    --output _generated/nodex_envelopes.py.new \
    --output-model-type pydantic_v2.BaseModel
diff -q _generated/nodex_envelopes.schema.json{,.new} \
    && diff -q _generated/nodex_envelopes.py{,.new}
```

Failure = nodex envelope changed shape. Resolve by reviewing the diff,
re-running generation locally, and committing the new file.

## TypeScript (Zod via `json-schema-to-zod`)

`json-schema-to-zod` emits Zod schemas which are *runtime* validators
— a `safeParse` call validates the envelope at the point of parse,
matching the contract gate semantic that's compile-time in other
languages.

### Install

```bash
pnpm add -D json-schema-to-zod zod
```

### Generate

```bash
nodex export envelope-schema > _generated/nodex-envelopes.schema.json
pnpm json-schema-to-zod \
    --input _generated/nodex-envelopes.schema.json \
    --output _generated/nodex-envelopes.ts
```

### Consume

```ts
import { QueryIssuesEnvelopeSchema } from "./_generated/nodex-envelopes";

const raw = execSync("nodex query issues", { encoding: "utf8" });
const parsed = QueryIssuesEnvelopeSchema.safeParse(JSON.parse(raw));
if (!parsed.success) {
    // Envelope shape doesn't match the schema generated from this
    // version of nodex — pin or regenerate.
    throw parsed.error;
}
const { orphans, stale } = parsed.data.data;
```

### Drift gate (CI step)

```bash
nodex export envelope-schema > _generated/nodex-envelopes.schema.json.new
pnpm json-schema-to-zod \
    --input _generated/nodex-envelopes.schema.json.new \
    --output _generated/nodex-envelopes.ts.new
diff -q _generated/nodex-envelopes.schema.json{,.new} \
    && diff -q _generated/nodex-envelopes.ts{,.new}
```

## Recommended layout

Per-project conventions vary, but every reference implementation
groups the generated artefacts under one `_generated/` (or
`generated/`) directory adjacent to the consuming code:

```
project/
├── _generated/
│   ├── README.md                  # "do not edit; regen via just <recipe>"
│   ├── nodex_envelopes.schema.json   # committed: nodex's own output, unmodified
│   └── nodex_envelopes.<py|ts>       # committed: codegen output
├── scripts/   (Python) | tools/   (TS)
└── Justfile / package.json        # 1 recipe wires steps 1 + 2
```

The `schema.json` artefact is committed alongside the generated code
so a reviewer can see what nodex's exact output looked like at the
commit's point in time, independent of which nodex version is on the
reviewer's machine.

## Notes on the schema shape

- The envelope is a `oneOf` of two branches: `{ok:true, data, warnings?}`
  and `{ok:false, error:{code, message}}`. Most codegen tools emit a
  discriminated union over the `ok` field.
- Each per-command entry's schema has its `$defs` lifted to the root,
  so external validators resolve all references in a single pass.
  No multi-file schema bundling is needed.
- `data.version` (on the `envelope-schema` manifest itself) carries
  the producing nodex version. Use it for visible drift markers in
  generated file headers if your codegen tool supports them.

## When *not* to codegen

Codegen has a cost: one new tool dependency, one CI step, one
generated file in the repo. It's worth it when the consumer is
*runtime-coupled* to the contract (gate scripts, CI validators,
production wrappers). It's overhead when the consumer is *one-shot*
(ad-hoc dashboard, prototype, throwaway script) — there, hand-rolled
`json.loads` + key access is fine. Use judgement; the schema is
available for both modes.

## Version pinning

Pair codegen with `nodex --check-version "<semver-req>"` at every
shell-out site. The check rejects a binary outside the pin range
before any envelope hits the consumer, so the codegen-generated
client never sees output it wasn't generated for.

```bash
nodex --check-version ">=0.8,<0.9" query annotations ...
```
