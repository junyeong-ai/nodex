[![Rust](https://img.shields.io/badge/rust-1.95.0-orange?logo=rust)](https://www.rust-lang.org)
[![Edition](https://img.shields.io/badge/edition-2024-blue)](https://doc.rust-lang.org/edition-guide/rust-2024/)
[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)

# nodex

> **[English](README.md)** | **한국어**

**마크다운 파일을 조회·검증·diff 가능한 문서 그래프로.**

nodex 는 프로젝트의 markdown 파일들을 스캔해 YAML frontmatter 와 링크 관계를 추출하고, 불변 그래프를 빌드해서 JSON-first CLI 로 조회·검증·diff·report 합니다. 에이전트·서버·AI 의존성 없는 순수 Rust 바이너리, 안정된 JSON 컨트랙트.

---

## 목차

1. [문제 정의](#문제-정의)
2. [빠른 시작](#빠른-시작)
3. [핵심 개념](#핵심-개념)
4. [동작 원리](#동작-원리)
5. [JSON-First CLI](#json-first-cli)
6. [검증 & Lifecycle](#검증--lifecycle)
7. [Diff & Export](#diff--export)
8. [설정](#설정)
9. [아키텍처](#아키텍처)
10. [설치](#설치)
11. [라이선스](#라이선스)

---

## 문제 정의

프로젝트 문서는 평평한 파일 더미가 아니라 그래프입니다. ADR-0002 가 ADR-0001 을 대체하고, 런북이 가이드에 의존하고, 스펙이 세 개의 룰로 구현됩니다. 하지만 이 그래프는 `[text](paths.md)` 와 frontmatter 필드 안에 암묵적으로 존재해 `grep` 과 `find` 로는 보이지 않습니다.

| 질문 | `grep` 의 한계 | 실제로 필요한 것 |
|---|---|---|
| "이 ADR 을 무엇이 대체했나?" | 텍스트가 아님 — supersession 추적 불가 | `superseded_by` forward walk |
| "이 문서에 무엇이 의존하나?" | 이름 매칭만, `related:` frontmatter 누락 | 모든 incoming edge |
| "어떤 문서가 고립됐나?" | 부재는 검색 불가 | incoming edge 0 인 노드 |
| "어떤 문서가 stale 인가?" | 날짜 비교 불가 | active + 리뷰 임계 초과 |
| "이 ref 간 무엇이 바뀌었나?" | 라인 diff 수준 | 추가/제거 노드, status 전이, field 변경 |
| "auth 문서 찾기" | 'auth' 포함 전체 | id/title/tag 가중치, 관계 컨텍스트 포함 |

nodex 는 그 암묵적 그래프를 명시화합니다. 한 번 파싱해서 인접 인덱스를 갖춘 타입 안전 in-memory 그래프를 만들고, 구조적 질문에 sub-millisecond 로 답합니다. 일상 워크플로 — pre-commit 검증, PR diff gate, 작성 전 중복 탐지, 외부 도구 vocabulary sync — 가 단일 JSON-emitting 명령으로 압축됩니다.

**핵심 속성:**

- **그래프, 폴더가 아님** — supersession chain, backlink, cross-reference 가 일급 시민
- **Config, 코드 아님** — 모든 프로젝트별 룰은 `nodex.toml`; 도메인 로직 하드코딩 0
- **증분 + 병렬** — Rust + rayon 병렬 read, SHA256 per-file 캐시로 변경된 파일만 재파싱
- **JSON-first 컨트랙트** — 모든 명령이 안정된 envelope (`{ok, data, warnings}` / `{ok, error: {code, message}}`) emit
- **순수 CLI** — 데몬·서버·AI/네트워크 의존성 없음, 모든 것이 동기 로컬 프로세스

---

## 빠른 시작

```bash
# 설치 (macOS / Linux)
curl -fsSL https://raw.githubusercontent.com/junyeong-ai/nodex/main/scripts/install.sh | bash

# 프로젝트에 config 초기화
nodex init

# 그래프 빌드
nodex build

# 문서 검색
nodex query search "auth"

# 관계 탐색
nodex query backlinks <node-id>
nodex query chain <node-id>

# 스키마 검증
nodex check

# 두 git ref 간 diff
nodex diff origin/main HEAD
```

모든 명령은 JSON 출력. `--pretty` 로 indented JSON.

---

## 핵심 개념

### 파일이 그래프가 된다

각 문서는 **노드**, 각 링크는 directed **edge** 가 됩니다.

### Edge 종류

| Source | 기본 relation | 예 |
|---|---|---|
| Frontmatter `supersedes` | `supersedes` | ADR 2 가 ADR 1 을 supersede |
| Frontmatter `implements` | `implements` | 룰이 ADR 을 구현 |
| Frontmatter `related` | `related` | 가이드가 ADR 과 관련 |
| 본문 링크 `[text](path.md)` | `references` | 본문에서 다른 문서 참조 |
| 커스텀 패턴 (config) | **임의 문자열** | 예: `@path.md` → `imports` |

`[[parser.link_patterns]]` 로 임의 relation 이름을 정의할 수 있습니다 — regex + relation 문자열 쌍.

본문 링크는 [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) AST 로 추출되므로 fenced code block 내부 링크는 무시됩니다.

### Frontmatter 스키마

| Field | Type | 필수 | 의미 |
|---|---|---|---|
| `id` | string | yes (path 로 추론 가능) | 노드 식별자 |
| `title` | string | yes | 사람이 읽는 이름 |
| `kind` | string | yes (추론 가능) | 문서 타입 — `[kinds].allowed` 에 있어야 함 |
| `status` | string | yes | lifecycle state — `[statuses].allowed` 에 있어야 함 |
| `created` / `updated` / `reviewed` | date (ISO) | optional | 각각 작성/수정/마지막 리뷰 |
| `owner` | string | optional | 소유자 식별자 |
| `supersedes` / `superseded_by` / `implements` / `related` | string \| array | optional | 관계 ID |
| `tags` | array | optional | 임의 태그 |
| `covers` | string \| array | optional | 이 문서가 권위를 주장하는 소스 코드 경로 |
| `orphan_ok` | bool | optional (기본 false) | orphan 경고 억제 |
| (그 외) | any | optional | `attrs` 에 저장, `[schema].mode = "strict"` 일 때는 거부 |

배열 필드는 단일 문자열과 배열 두 형태 모두 받습니다.

---

## 동작 원리

### Build 파이프라인

| 단계 | 내용 | 모듈 |
|---|---|---|
| **Scan** | `[scope].include` / `exclude` glob + `conditional_exclude` (terminal-status 부모 하위 skip) | `builder/scanner.rs` |
| **Cache** | `_index/cache.json` 로드. config-serialization SHA256 또는 `nodex` 바이너리 버전이 바뀌면 캐시 wholesale 무효 | `builder/cache.rs` |
| **Read** | `rayon::par_iter` 병렬 read. IO 에러는 fatal 이 아닌 warning | `builder/mod.rs` |
| **Parse** | per-file SHA256 hit/miss. miss 시 YAML frontmatter + pulldown-cmark 본문 + 커스텀 patterns 병렬 파싱 | `parser/` |
| **Dedupe IDs** | 같은 node id 두 문서면 `DUPLICATE_ID` 로 build 거부 | `builder/mod.rs` |
| **Resolve** | path → node id. 엄격 매칭. 미해결은 `ResolvedTarget::Unresolved { raw, reason }` 로 보존 | `builder/resolver.rs` |
| **Validate** | iterative 3-color DFS 로 `supersedes` cycle 검출 | `builder/validator.rs` |
| **Graph** | 결정적 정렬 후 불변 `Graph` 생성, 인접 인덱스 사전 빌드 | `model/graph.rs` |

빌드 후 `_index/graph.json` 작성. backlinks 는 derived state — edges 에서 O(degree) 로 매번 계산.

### 한 번 인덱스, 여러 번 조회

- **빌드 아티팩트**: `graph.json` — single source of truth
- **조회**: `graph.json` 만 읽음, 원본 markdown 재접근 없음, sub-millisecond 응답
- **증분**: SHA256 per file. `--full` 로 강제 fresh build

### Query 알고리즘

| Query | 결과 | 알고리즘 | 복잡도 |
|---|---|---|---|
| `search <kw>` | id/title/tag 매칭 + 점수 | substring 가중 점수 | O(n·m) |
| `backlinks <id>` | target 으로 들어오는 노드 | `incoming_indices(id)` 룩업 | O(degree_in) |
| `chain <id>` | supersession chain | `superseded_by` forward walk | O(chain_length) |
| `nodes [--kind --status --tag]` | 모든 술어 만족 노드 | linear filter, ranking 없음 | O(n·k) |
| `node <id> \| --path` | 노드 + incoming/outgoing | id 룩업 (직접) / path (linear) + 양쪽 인접 | O(degree), path는 O(n) |
| `orphans` | incoming 0 노드 | linear + `orphan_grace_days` | O(n) |
| `stale` | active + `reviewed` 임계 초과 | linear + 날짜 필터 | O(n) |
| `recent` | 날짜 윈도우 내 문서 | linear + 날짜 필터 | O(n) |
| `similar` | 점수 정렬 후보 | token Jaccard + tag/kind/dir/neighbour overlap | O(n·m) |
| `trust <id>` | 합성 신뢰도 + components | 4개 컴포넌트 가중 평균 | O(degree) |
| `components` | 연결 컴포넌트 분할 | undirected BFS, 결정적 정렬 | O(n + e) |
| `neighborhood <id>` | N홉 내 노드 | bounded BFS (undirected) | O(visited) |
| `covered-by <path>` | `covers:` 선언 문서 | linear scan | O(n) |
| `issues` | orphans + stale + unresolved + violations + skipped_rules | 위 + `check` 합성 | O(n + e) |

---

## JSON-First CLI

모든 명령은 stdout 에 JSON 출력. 사람이 읽는 텍스트는 `--help` / `--version` 만.

### Envelope

**Success:**
```json
{ "ok": true, "data": { /* ... */ }, "warnings": ["..."] }
```
- 비어있으면 `warnings` 생략
- 모든 list query 는 `data: { items: [...], total: N }`

**Error:**
```json
{ "ok": false, "error": { "code": "ERROR_CODE", "message": "..." } }
```

Error code 는 typed `nodex_core::error::Error` 의 `downcast_ref` 로 도출 — 메시지 문자열 매칭 금지.

| Code | 원인 |
|---|---|
| `CYCLE_DETECTED` | `supersedes` cycle |
| `DUPLICATE_ID` | 동일 node id 가 두 문서에 |
| `PARSE_ERROR` | YAML frontmatter / graph.json 손상 |
| `INVALID_TRANSITION` | lifecycle 액션이 허용 안 되는 status 에서 시도됨 |
| `NOT_FOUND` | 참조한 node id 가 그래프에 없음 |
| `ALREADY_EXISTS` | `scaffold` / `rename` 대상 경로 이미 존재 |
| `PATH_ESCAPES_ROOT` | `..` / 심볼릭 링크가 프로젝트 root 벗어남 |
| `CONFIG_ERROR` | `nodex.toml` load-time validation 실패 |
| `IO_ERROR` | filesystem read/write 실패 |
| `VERSION_MISMATCH` | `--check-version <req>` 가 바이너리 버전과 불일치 |
| `GIT_ERROR` | `git` 호출 실패 (work tree 없음, ref 부재 등) — `diff` / `check --since` 가 surface |
| `INVALID_ARGUMENT` | clap 파싱 실패 |
| `INTERNAL_ERROR` | 미분류 (버그) |

### Exit Code

| Code | 의미 |
|---|---|
| `0` | 성공 |
| `1` | `nodex check` 가 `severity = error` 위반 발견 |
| `2` | 런타임 실패 — error envelope 발생 |

### 전역 플래그

| Flag | 효과 |
|---|---|
| `-C DIR` | `git -C` 처럼 `DIR` 에서 시작한 것처럼 동작 |
| `--pretty` | JSON pretty-print |
| `--check-version <REQ>` | 바이너리 버전이 SemVer 요구사항을 만족하지 않으면 거부 (CI pin) |

### 명령 참조

| 명령 | 설명 |
|---|---|
| `nodex init` | `nodex.toml` 생성 (주석 포함 기본) |
| `nodex build [--full]` | 그래프 빌드; `--full` 은 캐시 무시 |
| `nodex check [--severity error\|warning] [--since <ref>]` | 검증 룰 실행; `--since` 는 변경된 노드만 + diff-aware 룰 활성; error 시 exit 1 |
| `nodex diff <ref-a> <ref-b>` | 두 git ref 간 구조 delta |
| `nodex report [--format md\|json\|all]` | `GRAPH.md` + `graph.json` 생성 |
| `nodex migrate [--apply]` | 레거시 문서에 frontmatter 주입 (기본 dry-run) |
| `nodex rename <old> <new>` | 파일 이동 + 본문 링크 일괄 재작성 |
| `nodex scaffold --kind X --title "..." [...]` | 유효한 frontmatter 로 신규 문서 생성 |
| `nodex query search <keyword> [--status x,y]` | id, title, tags 검색 |
| `nodex query backlinks <id>` | 대상으로 들어오는 모든 노드 |
| `nodex query chain <id>` | supersession chain |
| `nodex query orphans` | incoming edge 0 노드 |
| `nodex query stale` | `stale_days` 초과한 active 문서 |
| `nodex query nodes [--kind K1,K2] [--status S1,S2] [--tag T1,T2 --all-tags] [--limit N]` | 모든 술어를 만족하는 노드 (카테고리간 AND, 카테고리내 OR). 빈 필터 = 전체 노드. 태그 매칭은 대소문자 무시 (모든 tag-소비 surface 동일 fold) |
| `nodex query node <id> \| --path <file>` | 노드 상세 + incoming + outgoing. `--path` 는 editor / IDE 통합을 위한 역참조 — `./`, 절대경로(프로젝트 루트 하위)도 normalise |
| `nodex query covered-by <path>` | `covers:` 로 선언한 문서 |
| `nodex query issues` | orphans + stale + unresolved + violations + skipped_rules 통합 |
| `nodex query low-trust [--threshold N --kind K]` | `trust.low_trust_threshold` 미만 노드 (per-component breakdown 포함). Terminal status 문서는 `status` 컴포넌트가 항상 0이라 같이 surface — focus 가 필요하면 `--kind` 로 좁힘. |
| `nodex query trust <id>` | 합성 신뢰도 + 항상 포함되는 컴포넌트 breakdown |
| `nodex query similar [--id <id> \| --title "<t>" --kind K] ...` | Vector-free 유사도 |
| `nodex query recent [--days N --field F --kind K --since ...]` | 최근 윈도우 |
| `nodex query components` | 연결 컴포넌트 분할 (undirected, 정책 없음) |
| `nodex query neighborhood <id> [--depth N]` | `<id>` 의 N홉 이웃 (undirected, 토큰 카운팅 없음) |
| `nodex query dependents <id> [--depth N --relations a,b]` | `<id>` 에 transitive하게 의존하는 모든 노드 (역방향 traversal) |
| `nodex query annotations [--name <pattern>] [--with-frontmatter f1,f2,...]` | `[[annotations]]` 본문 마커를 capture key 별로 그룹핑; `--with-frontmatter` 는 선택한 frontmatter 필드(빌트인 / 프로젝트 선언)를 각 source 에 enrich — consumer 가 파일 재독을 피할 수 있게 함 |
| `nodex lifecycle <action> <id> [--to id]` | 상태 전이: `supersede --to <new>`, `archive`, `deprecate`, `abandon`, `review` |
| `nodex export schema` | 프로젝트 frontmatter 의 JSON Schema (draft 2020-12) |
| `nodex export enums` | closed-vocabulary 매니페스트 (kinds, statuses, per-field enums) |
| `nodex export rules` | active-rule 매니페스트 (현재 config 하에서 실제 발화될 룰 + per-rule `params` payload) |
| `nodex export envelope-schema` | 모든 CLI envelope shape 의 JSON Schema (draft 2020-12) — 타입드 다운스트림 consumer 의 codegen 컨트랙트 |

---

## 검증 & Lifecycle

### 빌트인 룰

`nodex check` 가 모든 등록된 룰을 그래프에 대해 실행. 응답에 `skipped_rules: [{rule_id, reason}]` 도 포함 — silent skip 금지.

| `rule_id` | Severity | 검사 내용 |
|---|---|---|
| `required_field` | error | 필수 필드 존재 |
| `field_type` | error | `attrs` 값이 선언된 `types` 와 일치 |
| `field_enum` | error | `attrs` + `kind` + `status` 가 선언된 `enums` 에 |
| `cross_field` | error | 조건부 요구 |
| `unknown_field` | error | 선언 안 된 frontmatter 키 (strict 모드만) |
| `filename_pattern` | error | 파일명이 `[[rules.naming]].pattern` 매치 |
| `sequential_numbering` | warning | 선두 자리 시퀀스에 gap 없음 |
| `unique_numbering` | warning | 두 파일이 같은 선두 prefix 공유 안 함 |
| `stale_review` | warning | active 노드가 `stale_days` 내 리뷰됐는지 |
| `git_drift` | warning | 참조 소스 파일이 `reviewed` 이후 변경됐는지 (opt-in) |
| `frontmatter_immutable/<name>` | error | `[[rules.frontmatter_immutable]]` 블록당 1개 — terminal 노드의 locked 필드 변경 (diff-aware, `check --since` 필요) |
| `body_immutable/<name>` | error | `[[rules.body_immutable]]` 블록당 1개 — terminal 노드 body 편집; `mode = "frozen"` 은 어떤 변경도 거부, `mode = "append_only"` 는 pre-terminal body 가 새 body 의 prefix 여야 함 (diff-aware) |
| `body_line/<name>` | error | `[[rules.body_line]]` 블록당 1개 — code block 밖에서 pattern 매치된 라인의 capture 값이 선언된 enum 안에 있어야 함 |

### Schema 모드

`[schema].mode`:
- `lenient` (기본): 선언 안 된 키는 `Node::attrs` 에 그대로
- `strict`: 빌트인 아니고 `types` / `enums` / `required` / `cross_field` 에도 없는 키면 `unknown_field` 위반 — 오타 차단

### Lifecycle 액션

`nodex lifecycle <action> <node-id>` 만이 status 를 변경하는 안전한 경로.

| Action | 결과 `status` | 기타 쓰는 필드 |
|---|---|---|
| `supersede --to <new-id>` | `superseded` | `superseded_by: <new-id>` |
| `archive` | `archived` | — |
| `deprecate` | `deprecated` | — |
| `abandon` | `abandoned` | — |
| `review` | (변경 없음) | `reviewed: <today>` |

네 개의 target status 는 **terminal** — 더 이상 lifecycle 이동 안 됨.

### Diff-aware 검증

`nodex check --since <ref>` 는 named ref 시점의 그래프를 `git worktree add --detach` 로 빌드하고, 구조 diff 를 계산해, 변경된 노드로만 violation 필터(neighbour 확장 없음, **순수 set 멤버십**) 한 뒤, 두 스냅샷 의미가 필요한 룰을 활성화:

- `frontmatter_immutable/<name>` — terminal 도달 후 선언 frontmatter 필드 잠금. 다중 블록 지원, 각 블록은 unique `name` + `fields` + 선택적 `kinds` 필터.
- `body_immutable/<name>` — terminal 도달 후 document body 잠금. `mode = "frozen"` 은 어떤 body 편집도 거부; `mode = "append_only"` 는 pre-terminal body 가 새 body 의 prefix 로 유지될 것을 요구. 빌드 시 계산된 per-node body fingerprint (whole-body SHA-256 + per-line hash vector) 로 구동 — check 시점 파일 재읽기 없음. 단순한 whole-body 잠금이 대상이며, nuanced edit 정책 (예: "`## Status` 섹션만 frontmatter 미러 허용") 같은 케이스는 프로젝트 자체 도구에 둘 것.

`--since` 없으면 두 패밀리 모두 `skipped_rules` 에 reason 과 함께 자기 보고 (silent pass 금지).

### Kind 필터

per-block 룰 패밀리 (`[[rules.body_line]]`, `[[rules.body_immutable]]`, `[[rules.frontmatter_immutable]]`) + `[[annotations]]` 모두 선택적 `kinds: ["..."]` 리스트 수용. 빈 리스트 = 제한 없음; 그렇지 않으면 `kind` 가 리스트에 있는 노드만 fire. 모든 엔트리는 `kinds.allowed` 에 있어야 하며 `Config::load` 가 typo 거부.

### 바이너리 버전 핀

`nodex.toml` 의 `[meta] nodex_version = ">=0.10, <0.11"` 이 설정되면 `Config::load` 는 실행 바이너리가 SemVer 요구를 만족하지 않으면 반환 거부 (error code `VERSION_MISMATCH`). 모든 CI / 컨트리뷰터가 자체 버전 검사를 다시 짤 필요 없이 프로젝트가 자기 도구 버전을 핀. 글로벌 `--check-version` CLI 플래그와 조합 — CLI 플래그는 config load 전에 더 먼저 검사.

---

## Diff & Export

### 구조 diff

```bash
nodex diff <ref-a> <ref-b>
```

각 ref 의 그래프를 `git worktree add --detach` 로 빌드 후 결정적 delta emit:

```json
{
  "added_nodes":   [...],
  "removed_nodes": [...],
  "added_edges":   [...],
  "removed_edges": [...],
  "status_transitions":   [{"id": "...", "from": "...", "to": "..."}],
  "field_changes":        [{"id": "...", "field": "...", "before": ..., "after": ...}],
  "added_annotations":    [...],
  "removed_annotations":  [...]
}
```

순수 구조 primitive — 정책·휴리스틱 없음. `check --since` 와 `frontmatter_immutable` / `body_immutable` 의 토대.

두 ref 모두 **현재** `nodex.toml` 로 파싱됩니다 (각 ref 시점의 `nodex.toml` 이 아님). 의도된 동작 — vocabulary 변경 (예: `kinds.allowed` 에서 값 제거) 이 영향받는 노드의 구체적 field change 로 표면화되어, 호환 안 되는 스키마 사이의 apples-to-oranges diff 를 생성하지 않습니다.

### 권위 매니페스트

```bash
nodex export schema           # frontmatter JSON Schema (draft 2020-12)
nodex export enums            # kinds + statuses + per-field enums
nodex export rules            # active rules (built-in + config-driven) + `params`
nodex export envelope-schema  # 모든 CLI envelope shape 의 JSON Schema (타입드 codegen 컨트랙트)
```

의존 방향 고정: nodex 가 emit, 외부 도구(TypeScript lint, IDE 플러그인, CI sync gate) 가 consume. 역방향 없음 — nodex 가 외부 파일을 파싱해 자체 vocabulary 도출하는 일은 없음.

`export envelope-schema` 는 codegen 컨트랙트입니다: 각 per-command 항목은 `$defs` 가 인라인된 자기 완결적 draft-2020-12 스키마라, 외부 consumer 가 nodex 가 emit 하는 shape 에서 곧장 타입을 생성합니다 (직접 손으로 미러링하지 않음). 매니페스트의 `version` 필드는 nodex 의 source-of-truth 버전이므로 CI gate 가 API 스키마 drift 처럼 envelope drift 도 검출 가능합니다.

---

## 설정

```toml
[scope]
include = ["docs/**/*.md", "specs/**/*.md", "README.md"]
exclude = ["docs/_index/**"]

[kinds]
allowed = ["generic", "guide", "readme", "adr"]

[statuses]
allowed = ["draft", "active", "superseded", "archived", "deprecated", "abandoned"]
terminal = ["superseded", "archived", "deprecated", "abandoned"]

[[identity.kind_rules]]
glob = "docs/decisions/**"
kind = "adr"

[[identity.id_rules]]
kind = "adr"
template = "adr-{stem}"

[[parser.link_patterns]]
pattern = "@([A-Za-z0-9_./-]+\\.md)"
relation = "imports"

[[rules.naming]]
glob = "docs/decisions/**"
pattern = "^\\d{4}-[a-z0-9-]+\\.md$"
sequential = true
unique = true

[rules.frontmatter_immutable]
fields = ["id", "kind", "superseded_by"]

# 본문 라인의 vocabulary 일치 강제. matched 라인의 capture 값이
# 선언된 enum 안에 있어야 함; 미매치 라인은 조용히 무시 (presence
# 룰이 아닌 conformance 룰).
# [[rules.body_line]]
# name = "spec-decision-log"
# pattern = '''^- \*\*(?P<gate>[a-z-]+)\*\*'''
# kinds = ["spec"]
# enums.gate = ["scope", "design", "rollout", "ship"]

# 본문 마커 추출 — `nodex query annotations` 로 surface.
# 그래프 노드로 resolve 안되는 pre-graph 식별자
# (TODO 토픽, promotion 후보, open research 질문).
# [[annotations]]
# name = "promotes"
# pattern = '''\[PROMOTES:\s*(?P<id>[\w-]+)\]'''
# key = "id"
# kinds = ["learning"]

[schema]
required = ["id", "title", "kind", "status"]
mode = "lenient"
cross_field = [
  { when = "status=superseded", require = "superseded_by" },
]

[[schema.overrides]]
kinds = ["adr"]
required = ["id", "title", "kind", "status", "decision_date"]
types = { decision_date = "date" }
enums = { priority = ["low", "medium", "high"] }

[detection]
stale_days = 180
orphan_grace_days = 14

[output]
dir = "_index"

[trust]
weights = { status = 0.4, freshness = 0.3, drift = 0.2, backlinks = 0.1 }
low_trust_threshold = 0.5

[similarity]
threshold = 0.3
default_limit = 10
weights = { title = 0.4, tags = 0.2, kind = 0.1, directory = 0.1, linked = 0.2 }
```

| Section | 제어 대상 |
|---|---|
| `[scope]` | 스캔 대상 파일 (`include` / `exclude` globs, `conditional_exclude`, `include_hidden` — dot 접두 경로는 기본 제외) |
| `[kinds]` | 허용된 `kind` 값 (`"generic"` 포함 필수) |
| `[statuses]` | 허용된 `status` 값 + terminal 목록 |
| `[identity]` | `kind_rules` + `id_rules` (template: `{stem}`, `{parent}`, `{kind}`, `{path_slug}`) |
| `[parser]` | 커스텀 `link_patterns`, 확장자, wikilink 토글 |
| `[rules]` | `naming` 패턴 + `frontmatter_immutable` lock 목록 + `body_line` 본문 vocabulary 검사 |
| `[[annotations]]` | 본문 마커 패턴 (regex + named-capture key); `query annotations` 로 surface |
| `[schema]` | `required` / `types` / `enums` / `cross_field` + per-kind `overrides` + `mode` |
| `[detection]` | `stale_days` / `orphan_grace_days` / `orphan_ok_kinds` / 선택적 `git_drift_threshold` |
| `[output]` | 빌드 아티팩트 위치 |
| `[report]` | `GRAPH.md` 포맷 limit |
| `[trust]` | 합성 점수 가중치 + low-trust 임계 |
| `[similarity]` | 유사도 임계, 기본 limit, 가중치, stop words |

---

## 아키텍처

### 워크스페이스

```
nodex/
├── nodex-core/    라이브러리 — 모든 로직
└── nodex-cli/     바이너리 — clap CLI; JSON envelope thin wrapper
```

### nodex-core 모듈

| 모듈 | 책임 |
|---|---|
| `model/` | 데이터 타입 — `Node`, `Edge`, `Graph`, `Kind`, `Status`, `ResolvedTarget`, `RawEdge`, `Annotation`, `RawAnnotation`, `BodyLineMatch`, `RawBodyLineMatch` |
| `parser/` | markdown → `(Node, Vec<RawEdge>, Vec<RawAnnotation>, Vec<RawBodyLineMatch>)`; YAML frontmatter, 본문 링크 (pulldown-cmark AST), `iter_body_lines` fence-aware iterator, identity 추론, 최소-diff `FrontmatterEditor` |
| `builder/` | scan → cache → read → parse → resolve → validate → graph |
| `query/` | read-only traversal: `search`, `traverse`, `detect`, `structure`, `issues`, `recent`, `similar` (`compute_similarity`), `trust` (`compute_trust`), `annotations` (`find_annotations`), `dependents` (`find_dependents`) |
| `diff.rs` | `compute_diff(before, after)` — 순수 구조 delta primitive |
| `export.rs` | `export_schema(&Config)` + `export_enums(&Config)` + `export_rules(&Config)` + `export_envelope_schema()` — authoritative manifests |
| `rules/` | `Rule` trait + 빌트인; `is_applicable` / `skip_reason` 가 diff-aware 룰 노출; `check` 가 `{violations, skipped}` 반환 |
| `command_result.rs` | 모든 명령의 typed `data` payload (`LifecycleResult`, `MigrateResult`, `RenameResult`, `InitResult`, `ReportResult`, `BuildResult`, `CheckResult`) — `export envelope-schema` 가 single SoT로 derive |
| `output/` | `graph.json` + 결정적 `GRAPH.md` |
| `lifecycle.rs` | frontmatter 를 수정하는 상태 전이 |
| `scaffold.rs` | 유효 frontmatter 신규 문서; similarity 로 deduplication |
| `path_guard.rs` | `..` / symlink 거부; canonical `write_atomic` |
| `config.rs` | `nodex.toml` load + validate; `Config::declared_fields_for(kind)` 가 strict 모드 구동 |
| `error.rs` | typed `Error` enum + 안정된 `code()` 문자열 |

### 설계 원칙

1. **불변 그래프.** `Graph` 는 한 번 빌드, 절대 mutate 안 됨.
2. **Config over code.** 프로젝트별 모든 것은 `nodex.toml`. core 는 도메인 지식 0.
3. **타입 안전 edge resolution.** `ResolvedTarget` 가 미해결을 명시적으로 보존.
4. **SHA256 증분 + 버전 무효화.** per-file content hash + config hash + 바이너리 버전 = 캐시 키.
5. **대칭적 mutation guard.** disk 에 쓰는 모든 명령이 `path_guard` 경유.
6. **No silent rule skip.** fire 하지 않는 룰은 `skipped_rules` 에 reason 과 함께 등장.
7. **One-way export.** nodex 가 emit, 외부 도구가 consume. dependency 방향 고정.

메타 invariant: **nodex 가 직접 쓰는 모든 문서는 nodex 자기 `check` 를 통과해야 함.** [`.claude/rules/config-driven.md`](.claude/rules/config-driven.md) 참조.

---

## 설치

### 빠른 설치

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/junyeong-ai/nodex/main/scripts/install.sh | bash

# Windows (PowerShell)
iwr -useb https://raw.githubusercontent.com/junyeong-ai/nodex/main/scripts/install.ps1 | iex
```

### 지원 플랫폼

| OS | Architecture | Target |
|---|---|---|
| Linux | x86_64 | `x86_64-unknown-linux-musl` (static) |
| Linux | arm64 | `aarch64-unknown-linux-musl` (static) |
| macOS | Intel + Apple Silicon | `universal-apple-darwin` |
| Windows | x86_64 | `x86_64-pc-windows-msvc` |

### 소스 빌드

```bash
git clone https://github.com/junyeong-ai/nodex
cd nodex
cargo install --path nodex-cli
```

### CI 핀

```bash
nodex --check-version ">=0.8,<0.9" build
```

---

## 라이선스

MIT

---

> **[English](README.md)** | **한국어**
