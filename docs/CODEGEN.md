# Generating Typed Clients from `nodex export envelope-schema`

`nodex export envelope-schema` emits a draft 2020-12 JSON Schema for
every CLI envelope shape. It is the **canonical contract** between
nodex and any downstream that parses its JSON output — types
generated from it cannot drift from what the CLI actually serialises,
because the schema is derived from the same Rust structs that produce
the JSON.

This guide is the recommended way to consume that schema. Skipping it
and hand-rolling envelope types is supported but loses the contract
gate; nodex's release CI diffs every release's envelope schema against
the previous release's published asset and fails the release unless a
classified change carries the promised minor-or-major bump, so
consumers without a regeneration step learn about drift at upgrade
time, not runtime. Every release publishes
`nodex-envelope-schema-v<ver>.json` and `nodex-commands-v<ver>.json`
as assets — pin against them instead of regenerating locally when you
want an attested contract artifact.

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
nodex export envelope-schema → extract the per-command schema → [codegen] → typed payload models → CI drift gate
```

`export envelope-schema` emits the full registry wrapped in the CLI
envelope: `{ok, data: {version, envelope, per_command}}`. `data.envelope`
is the generic `{ok, data, warnings} / {ok, error}` shape and
`data.per_command["<cmd>"]` is the self-contained draft-2020-12 schema of
that command's `data` payload. Codegen tools consume **one root schema at
a time**, so the Generate step extracts the entry you consume (with `jq`
below — the one extra dependency every Generate / CI block uses) rather
than piping the whole registry — fed the registry raw, every generator
degenerates to an untyped catch-all.

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

One extraction + one codegen per command you consume; the root model
name comes from `--class-name` (the schema's own `title` would name the
inner item type otherwise):

```bash
nodex export envelope-schema > _generated/nodex_envelope_schema.json
jq '.data.per_command["query.annotations"]' \
    _generated/nodex_envelope_schema.json \
    > _generated/query_annotations.schema.json
uv run datamodel-codegen \
    --input _generated/query_annotations.schema.json \
    --input-file-type jsonschema \
    --class-name QueryAnnotationsData \
    --output _generated/query_annotations.py \
    --output-model-type pydantic_v2.BaseModel
```

### Consume

Check the envelope's `ok` discriminant, then validate the `data`
payload with the generated model:

```python
import json, subprocess
from _generated.query_annotations import QueryAnnotationsData

proc = subprocess.run(
    ["nodex", "query", "annotations", "--with-frontmatter", "created,tags"],
    capture_output=True, text=True, check=True,
)
envelope = json.loads(proc.stdout)
if not envelope["ok"]:
    raise RuntimeError(envelope["error"]["code"])
data = QueryAnnotationsData.model_validate(envelope["data"])
for group in data.items:
    for entry in group.entries:
        for source in entry.sources:
            # Built-in fields are typed.
            print(source.source, source.line)
            # Project-declared frontmatter stays a dict — the schema
            # cannot know per-project keys (e.g. `priority`, `owner`).
            created = source.frontmatter.get("created")
```

### Drift gate (CI step)

```bash
nodex export envelope-schema > _generated/nodex_envelope_schema.json.new
jq '.data.per_command["query.annotations"]' \
    _generated/nodex_envelope_schema.json.new \
    > _generated/query_annotations.schema.json.new
uv run datamodel-codegen \
    --input _generated/query_annotations.schema.json.new \
    --input-file-type jsonschema \
    --class-name QueryAnnotationsData \
    --output _generated/query_annotations.py.new \
    --output-model-type pydantic_v2.BaseModel
diff -q _generated/nodex_envelope_schema.json{,.new} \
    && diff -q _generated/query_annotations.py{,.new}
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

`json-schema-to-zod` does not follow `$ref`, so feed it the
self-contained emission form: `--inline-refs` resolves every
`#/$defs/...` reference in place (fail-closed in the producer), and the
extracted schema needs no pre-processing. The default `$defs`-bundled
form stays the right input for named-model generators like the Python
path above, whose model class names derive from the `$defs` keys.

```bash
nodex export envelope-schema --inline-refs > _generated/nodex-envelope-schema.json
jq '.data.per_command["query.issues"]' \
    _generated/nodex-envelope-schema.json \
    > _generated/query-issues.schema.json
pnpm json-schema-to-zod \
    --input _generated/query-issues.schema.json \
    --name QueryIssuesDataSchema \
    --output _generated/query-issues.ts
```

### Consume

Check the envelope's `ok` discriminant, then validate the `data`
payload with the generated schema:

```ts
import { QueryIssuesDataSchema } from "./_generated/query-issues";

const raw = execSync("nodex query issues", { encoding: "utf8" });
const envelope = JSON.parse(raw);
if (!envelope.ok) {
    throw new Error(envelope.error.code);
}
const parsed = QueryIssuesDataSchema.safeParse(envelope.data);
if (!parsed.success) {
    // Payload shape doesn't match the schema generated from this
    // version of nodex — pin or regenerate.
    throw parsed.error;
}
const { orphans, stale } = parsed.data;
```

### Drift gate (CI step)

```bash
nodex export envelope-schema --inline-refs > _generated/nodex-envelope-schema.json.new
jq '.data.per_command["query.issues"]' \
    _generated/nodex-envelope-schema.json.new \
    > _generated/query-issues.schema.json.new
pnpm json-schema-to-zod \
    --input _generated/query-issues.schema.json.new \
    --name QueryIssuesDataSchema \
    --output _generated/query-issues.ts.new
diff -q _generated/nodex-envelope-schema.json{,.new} \
    && diff -q _generated/query-issues.ts{,.new}
```

## Recommended layout

Per-project conventions vary, but every reference implementation
groups the generated artefacts under one `_generated/` (or
`generated/`) directory adjacent to the consuming code:

```
project/
├── _generated/
│   ├── README.md                    # "do not edit; regen via just <recipe>"
│   ├── nodex_envelope_schema.json   # committed: the full export, unmodified (drift reference)
│   ├── query_issues.schema.json     # committed: extracted per-command schema
│   └── query_issues.<py|ts>         # committed: codegen output
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
- Each per-command entry's schema has its `$defs` lifted to that
  entry's root, so a spec-compliant validator resolves every `$ref` in
  a single pass — no multi-file bundling — and the `$defs` names drive
  named-model codegen. `--inline-refs` re-emits the same model fully
  self-contained (no `$ref` / `$defs` anywhere) for tools that do not
  follow references (notably `json-schema-to-zod`). Two emission forms,
  one canonical model.
- `data.version` (on the `envelope-schema` manifest itself) carries
  the producing nodex version. Use it for visible drift markers in
  generated file headers if your codegen tool supports them.

## Contract tests

Generated types protect the *parse* seam; a contract test protects the
*dispatch* seam — the code that picks fields off a payload whose shape
it assumed. Pin one unit test per consumed command that validates a
checked-in sample envelope (or a captured live response) against the
extracted per-command schema, so a shape guess fails at test time
instead of in production. The classic trap is reading a report-shaped
payload as an `{items, total}` list: `query node`, `query issues`,
`query trust <id>`, `query neighborhood`, and `query dependents` return
objects, not item lists — `export envelope-schema` is the authority,
never the shape of a neighbouring command. Pin the baseline against the
release asset (`nodex-envelope-schema-v<ver>.json`) when the consumer
should only move shapes deliberately.

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
nodex --check-version ">=0.18, <0.19" query annotations ...
```
