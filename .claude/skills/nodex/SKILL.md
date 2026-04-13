---
name: nodex
description: |
  Query and manage the document graph via nodex CLI. Use when searching docs,
  exploring relationships, checking validation, or applying lifecycle transitions.
argument-hint: "<command> [args]  e.g. query search auth, check, lifecycle review doc-1"
allowed-tools: Bash(nodex *)
---

# nodex — Document Graph CLI

Run `nodex` commands to query, validate, and manage the document graph.
All output is JSON. The graph must be built first with `nodex build`.

## Workflow

```
nodex build              # Build/refresh the graph (incremental, fast)
nodex query search ...   # Find documents
nodex check              # Validate rules
nodex lifecycle ...      # Apply state transitions
nodex report             # Generate GRAPH.md + JSON
```

## Query Commands

```bash
# Keyword search (searches id, title, tags)
nodex query search <keyword>
nodex query search <keyword> --status active

# Relationships
nodex query backlinks <node-id>    # Who links to this node?
nodex query chain <node-id>        # Supersession chain (oldest → newest)
nodex query node <node-id>         # Full detail with incoming/outgoing edges

# Detection
nodex query orphans                # Nodes with zero incoming edges
nodex query stale                  # Active docs past review threshold

# Tags
nodex query tags <tag1> <tag2>           # Match any tag
nodex query tags <tag1> <tag2> --all     # Match all tags
```

## Lifecycle Commands

```bash
nodex lifecycle review <node-id>                    # Refresh reviewed date
nodex lifecycle archive <node-id>                   # Mark as archived
nodex lifecycle deprecate <node-id>                 # Mark as deprecated
nodex lifecycle abandon <node-id>                   # Mark as abandoned
nodex lifecycle supersede <node-id> --to <new-id>   # Supersede with replacement
```

## Output Format

Every command returns:
```json
{"ok": true, "data": {...}}        // success
{"ok": false, "error": {...}}      // failure with code + message
```

Query results: `data.items` (array) + `data.total` (count).

## When to Use

- **Before writing new docs**: `nodex query search <topic>` to check what exists
- **Before code review**: `nodex check` to catch validation issues
- **After completing work**: `nodex lifecycle review <id>` to refresh review date
- **When replacing a doc**: `nodex lifecycle supersede <old> --to <new>`
