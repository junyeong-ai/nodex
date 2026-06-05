# Nodex 종합 재설계 및 구현 계획

**최종 검증 일자**: 2026-06-05  
**분석 범위**: 아키텍처 논리적 결함, 네이밍 일관성, 오탐 위험, 불필요한 복잡성  
**설계 원칙**: 하위호환성 제거, 처음부터 이렇게 설계된 것처럼 클린하게 최적화

---

## Part 1: 검증된 설계 강점 (유지)

### 1.1 ✅ 초석 아키텍처 (변경 금지)

```
✓ Config-driven 원칙 (모든 도메인 로직이 nodex.toml에서 정의)
✓ Typed Error enum (JSON envelope의 .code 필드로 분류 가능)
✓ Path safety (path_guard.rs 중앙집중식)
✓ RuleContext 격리 (규칙이 filesystem 직접 접근 금지)
✓ Self-consistency (scaffold/migrate/lifecycle이 생성한 문서 = check 통과)
✓ Symmetric validation (config load → validate → preflight 3단계)
✓ Merged config views (required_for, types_for, enums_for)
```

**유지 이유**: 이들이 없으면 다른 모든 개선이 무너짐. 깔끔한 기초.

### 1.2 ✅ 강한 타입 (유지)

```
✓ Kind(String), Status(String) newtype → 문자열 실수 불가능
✓ Node, Graph, Edge 강한 정의
✓ Action enum (컴파일러가 유효한 transition만 허용)
✓ RawEdge → ResolvedTarget 명시적 변환
```

### 1.3 ✅ 검증 체계 (강화)

```
✓ 69개 validation test 전부 통과
✓ CLAUDE.md의 "no silent runtime skips" 원칙 구현
✗ 그러나 일부 검증 gaps 있음 (다음 section에서 수정)
```

---

## Part 2: 확인된 논리적 결함 및 수정 계획

### 2.1 🔴 CRITICAL 발견 #1: Config Hash Semantic 불명확

**문제**:
```rust
// builder/mod.rs:85-89
let config_hash = crate::hash::sha256_hex(&format!(
    "nodex={}\n{}",
    env!("CARGO_PKG_VERSION"),
    config_json  // JSON direct 직렬화 → order-dependent
));
```

**현재 상태**: `serde_json::to_string(config)`는 deterministic (BTreeMap 정렬, Vec 순서 유지)
→ **id_rules 순서 변경 = config_hash 변경 = cache invalidation 자동 작동 ✓**

**문제점**: 이것이 **문서화되지 않음** → 운영자가 모름
- "id_rules 순서를 바꾸면 캐시가 자동으로 무효화된다"는 보장이 명시되지 않음
- 혹시 JSON 직렬화 순서가 변경되면? (예: 미래 serde_json 버전)

**수정 방안**:

```rust
// 1. Explicit semantic hash (config_json 대신 semantic components)
fn compute_config_hash(config: &Config) -> String {
    // TOML 구조가 아니라 semantic content를 hash
    let semantic = format!(
        "kinds:{:?}\nstatuses:{:?}\nid_rules:{:?}\nrules:{:?}",
        config.kinds.allowed,
        config.statuses.allowed,
        config.identity.id_rules,  // 순서 포함
        config.rules,
    );
    hash::sha256_hex(&format!(
        "nodex={}\n{}",
        env!("CARGO_PKG_VERSION"),
        semantic
    ))
}

// 2. Test: Config 순서 변경이 hash 변경 트리거
#[test]
fn config_hash_changes_when_id_rules_order_changes() {
    let mut config1 = default_config();
    let mut config2 = default_config();
    
    config2.identity.id_rules.reverse();  // 순서만 변경
    
    let hash1 = compute_config_hash(&config1);
    let hash2 = compute_config_hash(&config2);
    assert_ne!(hash1, hash2, "id_rules 순서 변경이 hash를 변경해야 함");
}

// 3. Document in nodex-core/CLAUDE.md:
// "BuildCache invalidation is triggered by:
//  1. nodex binary version change (env!("CARGO_PKG_VERSION"))
//  2. Config semantic change (id_rules order, identity.kind_rules order, etc.)
//  The cache is safe to use across config text reordering (whitespace, comment)
//  but NOT across semantic reordering (id_rules)."
```

**변경 범위**: `builder/mod.rs` (hash 계산 명시화) + 테스트 + 문서

---

### 2.2 🔴 CRITICAL 발견 #2: Git Drift Threshold 의미 모호

**문제**:
```rust
// query/trust.rs:176-180
if threshold == 0 {
    return Some(1.0);  // "drift 무시" vs "비활성화"?
}
```

**현재**: 
- rule::check (rules/git_drift.rs) → `is_applicable` 체크는 `threshold.is_some()` (0도 Some)
- query::trust → `threshold == 0`일 때 1.0 반환

**모순**: threshold = 0이면 rule은 fire하지만 trust는 최대 신뢰도 반환

**수정 방안**:

```rust
// 1. Config schema: threshold는 Option<u32>만 (0 불허)
// config.rs:
pub struct DetectionConfig {
    pub stale_days: Option<u32>,        // None = disabled, Some(n) = n days
    pub orphan_grace_days: u32,         // always active
    pub git_drift_threshold: Option<u32>, // None = disabled
    // 변경: git_drift_threshold가 0이 될 수 없음
}

// 2. Validator 강화
fn validate_detection(detection: &DetectionConfig) -> Result<()> {
    if let Some(stale) = detection.stale_days {
        if stale == 0 {
            return Err(Error::Config(
                "stale_days must be > 0 or None (disabled); got 0".into()
            ));
        }
    }
    if let Some(drift) = detection.git_drift_threshold {
        if drift == 0 {
            return Err(Error::Config(
                "git_drift_threshold must be > 0 or None (disabled); got 0".into()
            ));
        }
    }
    Ok(())
}

// 3. Rules and queries stay consistent
// is_applicable: "if threshold.is_none() return false"
// trust: "if threshold.is_none() return None (not applicable)"

// 4. Test
#[test]
fn validate_rejects_zero_stale_days() {
    let mut config = Config::default();
    config.detection.stale_days = Some(0);
    assert!(config.validate().is_err());
}

#[test]
fn validate_rejects_zero_git_drift_threshold() {
    let mut config = Config::default();
    config.detection.git_drift_threshold = Some(0);
    assert!(config.validate().is_err());
}
```

**변경 범위**: `config.rs` (schema) + `rules/git_drift.rs` (is_applicable) + `query/trust.rs` (일관성) + 테스트

---

### 2.3 🔴 CRITICAL 발견 #3: DAG 불변식이 부분적

**문제**: `supersedes` 관계는 DAG 강제, 하지만 `implements`, `related`, `covers` 관계는 cycle 감지 없음

**현재**: `builder/validator.rs::validate_supersedes_dag`는 supersedes만 검사

**수정 방안**:

```rust
// 1. Graph cycle detection rule (선택사항, config-driven)
// config.toml에서:
// [[rules.graph_invariants]]
// name = "no-cycles"
// kinds = ["*"]  // 모든 kind에 적용
// relations = ["implements", "covers", "depends_on"]  // 어떤 관계에서 cycle 금지할지

// 2. Rule 구현: rules/graph_invariants.rs (신규)
pub struct GraphCycleDetectionRule {
    pub relations: Vec<String>,  // 이 관계들에서만 cycle 검사
    pub severity: Severity,
}

impl Rule for GraphCycleDetectionRule {
    fn id(&self) -> &str { "graph_invariants/cycle-detection" }
    
    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let mut violations = Vec::new();
        
        for rel in &self.relations {
            // DFS로 cycle 감지
            let cycles = find_cycles_in_relation(ctx.graph, rel);
            for cycle in cycles {
                violations.push(Violation {
                    line: None,  // structural, no line number
                    message: format!("cycle detected in '{}' relation: {}", rel, cycle.join(" → ")),
                    severity: self.severity,
                });
            }
        }
        violations
    }
}

// 3. Optional config
#[serde(default)]
pub struct GraphInvariantsRule {
    pub kinds: Vec<String>,
    pub relations: Vec<String>,
    pub severity: Severity,
}

// 4. Register in rules::registered_rules if config.rules.graph_invariants is Some

// 5. Test
#[test]
fn detects_cycle_in_implements_relation() {
    let mut graph = ...;
    graph.add_edge("adr-a", "adr-b", "implements");
    graph.add_edge("adr-b", "adr-a", "implements");  // cycle
    
    let violations = check_cycle_detection(&graph);
    assert!(!violations.is_empty());
}
```

**변경 범위**: 신규 rule (rules/graph_invariants.rs) + config schema (rules.graph_invariants) + 테스트
**선택사항**: 이 규칙은 기본값으로는 비활성화 (config.rules.graph_invariants = None)

---

## Part 3: 높은 우선순위 개선 (논리 정확성)

### 3.1 🟡 HIGH 발견 #4: ID Rule 순서의 명시적 보장

**문제**: "First match wins" 정책이 문서화되지만, 규칙 순서 변경 시 node ID 변경 가능성

**수정 방안**:

```rust
// 1. Config validation: id_rules 순서 결정성 확인
fn validate_id_rules_determinism(id_rules: &[IdRule]) -> Result<()> {
    // 각 규칙의 glob과 kind가 겹치는지 확인
    // 만약 "docs/**/*.md" (kind=adr)과 "docs/decisions/*.md" (kind=adr)
    // 이 둘이 겹치면 경고 발생
    
    for (i, rule1) in id_rules.iter().enumerate() {
        for (j, rule2) in id_rules.iter().enumerate() {
            if i >= j { continue; }
            
            // pattern이 겹치고 kind도 같으면?
            if patterns_overlap(&rule1.glob, &rule2.glob)
                && rule1.kind == rule2.kind
            {
                return Err(Error::Config(format!(
                    "id_rules[{}] and id_rules[{}] have overlapping patterns \
                     with the same kind. Order matters (first match wins). \
                     Sort by specificity: place '{}' before '{}'",
                    j, i, rule2.glob, rule1.glob
                )));
            }
        }
    }
    Ok(())
}

// 2. Auto-sort option (config에서)
pub struct IdentityConfig {
    pub kind_rules: Vec<KindRule>,
    pub id_rules: Vec<IdRule>,
    #[serde(default)]
    pub id_rules_sort_by_specificity: bool,  // true면 longest-glob-first로 정렬
}

// 3. Document in CLAUDE.md:
// "id_rules[i] uses first-match-wins semantics.
//  To avoid ambiguous ordering:
//  - Set config.identity.id_rules_sort_by_specificity = true (auto-sort by glob specificity)
//  - OR manually order rules from most-specific to least-specific glob"

// 4. Test
#[test]
fn id_rules_order_is_deterministic_across_rebuilds() {
    let config = load_config_from_file("nodex.toml");
    
    let node1_id_before = infer_id("docs/decisions/0001.md", &Kind::new("adr"), &config);
    let node1_id_after = infer_id("docs/decisions/0001.md", &Kind::new("adr"), &config);
    
    assert_eq!(node1_id_before, node1_id_after);
}
```

**변경 범위**: `config.rs` (validation + optional auto-sort) + `parser/identity.rs` (sort 로직) + 문서

---

### 3.2 🟡 HIGH 발견 #5: Custom Link Pattern 캡처 그룹 명시화

**문제**: 여러 capture group을 선언해도 첫 번째만 사용 (silent behavior)

**수정 방안**:

```rust
// config.rs: link_pattern validation
fn validate_link_pattern(pattern: &str) -> Result<()> {
    let re = regex::Regex::new(pattern)?;
    let capture_count = re.captures_len();
    
    if capture_count == 0 {
        return Err(Error::Config(
            "link pattern must have at least one capture group".into()
        ));
    }
    
    if capture_count > 1 {
        return Err(Error::Config(format!(
            "link pattern must have exactly one capture group \
             (the link target); got {}: {}",
            capture_count, pattern
        )));
    }
    
    Ok(())
}

// parser/body.rs: explicit documentation
fn extract_links(...) {
    // link_patterns로부터 링크 추출
    // 각 pattern은 정확히 1개 capture group을 가져야 함 (validator에서 강제)
    // capture group의 콘텐츠가 link target
    
    for custom_pattern in &parser.link_patterns {
        // pattern은 이미 validated → 정확히 1 capture group
        let re = Regex::new(&custom_pattern.pattern)
            .expect("validated at config load time");
        
        if let Some(caps) = re.captures(line) {
            if let Some(target) = caps.get(1) {  // group 1 is the target
                edges.push(RawEdge::from_custom_link(...));
            }
        }
    }
}

// Test
#[test]
fn validate_rejects_link_pattern_with_zero_groups() {
    assert!(validate_link_pattern("mylink://(.*)").is_ok());  // 1 group: OK
    assert!(validate_link_pattern("mylink://.*").is_err());   // 0 groups: rejected
    assert!(validate_link_pattern("mylink://(.*)-(.*)").is_err());  // 2 groups: rejected
}
```

**변경 범위**: `config.rs` (validation 강화) + `parser/body.rs` (문서화) + 테스트

---

## Part 4: 중간 우선순위 개선 (운영 안정성)

### 4.1 🟠 MEDIUM 발견 #6: Conditional Exclude 상태 의미

**문제**: `scope.conditional_exclude` 조건이 "예상 status"를 기반으로 하는데, 이것이 문서화되지 않음

**현재 코드**:
```rust
pub struct ConditionalExclude {
    pub parent_glob: String,
    pub condition: String,  // "status_terminal"
}

// scanner에서 scan_time에 적용
// 그런데 이 시점에 status는?
// → default status (Config::initial_status_for(kind))
```

**수정 방안**:

```rust
// config.rs: 문서화
/// Exclude files at scan time based on expected (default) status.
/// 
/// Example:
/// ```toml
/// [[scope.conditional_exclude]]
/// parent_glob = "specs/*/detail"
/// condition = "status_terminal"
/// ```
/// 
/// Excludes files under "specs/*/detail" if their parent spec
/// would have terminal status (using the default status for "spec" kind).
/// This lets spec sub-files be cleanly excluded when their parent is archived.
///
/// WARNING: This uses the kind's DEFAULT status, not the actual status
/// in the frontmatter. If a file explicitly sets status: active in its
/// parent, the child file will still be excluded. This is intentional:
/// conditional_exclude happens before frontmatter parse.
pub conditional_exclude: Vec<ConditionalExclude>,

// scanner.rs: explicit implementation
fn should_exclude_by_condition(
    parent_path: &Path,
    condition: &str,
    root: &Path,
    config: &Config,
) -> Result<bool> {
    match condition {
        "status_terminal" => {
            // Infer the parent's kind
            let parent_kind = parser::identity::infer_kind(parent_path, config);
            
            // Get its default status
            let default_status = config.initial_status_for(&parent_kind);
            
            // Check if that status is terminal
            Ok(config.is_terminal_status(&default_status))
        }
        _ => {
            // Should never happen (validator rejects unknown conditions)
            unreachable!("unknown condition: {} (should have been rejected at load)", condition)
        }
    }
}

// Test
#[test]
fn conditional_exclude_uses_default_status_not_actual() {
    // If parent has status: active in frontmatter,
    // but default status for its kind is "archived",
    // it should still be excluded.
    // (This documents the current behavior and prevents regression.)
}
```

**변경 범위**: `config.rs` (문서화 강화) + `scanner.rs` (명시적 구현) + 테스트

---

### 4.2 🟠 MEDIUM 발견 #7: Kind 커버리지 검증

**문제**: `kinds.allowed`에 선언했지만 `identity.kind_rules`에서 미사용 → fallback generic 사용 → schema 불일치 가능

**예시**:
```toml
[kinds]
allowed = ["adr", "guide", "runbook", "generic"]

[[identity.kind_rules]]
glob = "docs/decisions/*.md"
kind = "adr"

# guide와 runbook은 rule이 없음!
# → docs/guides/intro.md → infer_kind → no match → generic (wrong!)
```

**수정 방안**:

```rust
// config.rs: coverage validation
fn validate_kind_rules_coverage(config: &Config) -> Vec<Warning> {
    let mut warnings = Vec::new();
    let declared_kinds: HashSet<&str> = config.kinds.allowed.iter().map(|s| s.as_str()).collect();
    let used_kinds: HashSet<&str> = config.identity.kind_rules
        .iter()
        .map(|r| r.kind.as_str())
        .collect();
    
    for unused in declared_kinds.difference(&used_kinds) {
        if unused != &FALLBACK_KIND {  // generic는 fallback이니까 OK
            warnings.push(Warning {
                topic: "kind_rules_coverage",
                message: format!(
                    "kind '{}' is allowed but has no identity.kind_rules glob. \
                     Files matching this kind would use fallback '{}'",
                    unused, FALLBACK_KIND
                ),
            });
        }
    }
    warnings
}

// config.rs: expose warnings from Config::load
pub fn load(root: &Path) -> Result<ConfigWithWarnings> {
    let config = Self::validate()?;
    let warnings = Self::compute_load_warnings(&config);
    Ok(ConfigWithWarnings { config, warnings })
}

// CLI: surface warnings
// nodex build 출력에:
// ⚠  kind_rules_coverage: kind 'guide' is allowed but has no identity.kind_rules glob
```

**변경 범위**: `config.rs` (warning computation) + CLI (warning display) + 테스트

---

## Part 5: AI-Native 기능 확장

### 5.1 🆕 AI 컨텍스트 쿼리 (P0)

**구현 계획**: [별도 상세 설계 문서]

```rust
// nodex-core/src/query/context.rs (신규)
pub struct ContextQuery {
    pub seed_id: String,
    pub depth: usize,
    pub include_body: bool,
}

pub struct ContextResult {
    pub seed: NodeRef,
    pub incoming: Vec<RelatedNode>,
    pub outgoing: Vec<RelatedNode>,
    pub narrative: String,
}

pub fn context(
    graph: &Graph,
    config: &Config,
    root: &Path,
    opts: ContextQuery,
) -> Result<ContextResult> { ... }

// nodex-cli: add command
// nodex query context <id> --depth 2 --include-body
```

**변경 범위**: 신규 모듈 (~600줄) + CLI 명령 (~150줄)

---

### 5.2 🆕 그래프 건강도 메트릭 (P1)

**구현 계획**: [별도 상세 설계 문서]

```rust
// nodex-core/src/query/metrics.rs (신규)
pub struct GraphMetrics {
    pub total_nodes: usize,
    pub orphan_count: usize,
    pub unresolved_edges: usize,
    pub supersession_chains: ChainMetrics,
    pub review_lag: ReviewLagMetrics,
}

pub fn compute_metrics(graph: &Graph, config: &Config) -> GraphMetrics { ... }

// nodex-cli: add command
// nodex report --metrics
```

**변경 범위**: 신규 모듈 (~400줄) + CLI 명령 (~100줄)

---

## Part 6: 코드 정리 (불필요한 복잡성)

### 6.1 설정 검증 모듈화

**현재**: `config.rs::validate()` 1500+ 줄 (모놀리식)

**계획**:
```rust
// config.rs: 기존 validate → 헬퍼들로 분해
pub fn validate(&self) -> Result<()> {
    self.validate_kinds()?;
    self.validate_statuses()?;
    self.validate_schema()?;
    self.validate_identity()?;
    self.validate_rules()?;
    self.validate_scope()?;
    self.validate_detection()?;
    self.validate_trust()?;
    self.validate_similarity()?;
    self.validate_output()?;
    Ok(())
}

// 각 validate_* 함수: 같은 파일 내부, 200줄 미만
fn validate_kinds(&self) -> Result<()> { ... }
fn validate_statuses(&self) -> Result<()> { ... }
// ...
```

**변경 범위**: `config.rs` (refactor, 기능 변경 없음)

---

## Part 7: 테스트 커버리지 강화

### 7.1 새로운 테스트 케이스

```
1. config_hash 의미론적 안정성 (발견 #1)
   - id_rules 순서 변경 → hash 변경
   - comment/whitespace 변경 → hash 동일

2. Zero threshold 거부 (발견 #2)
   - stale_days = 0 → 거부
   - git_drift_threshold = 0 → 거부

3. Cycle detection (발견 #3)
   - 각 관계(implements, related, covers)에서 cycle 감지

4. ID rules determinism (발견 #4)
   - 동일 config → 동일 ID

5. Link pattern validation (발견 #5)
   - 0 capture groups → 거부
   - 2+ capture groups → 거부

6. Kind coverage (발견 #7)
   - unused kind → warning
```

**추가 테스트 수**: ~30개

---

## Part 8: 문서화 개선

### 8.1 CLAUDE.md 강화

```markdown
### Config Semantics

#### git_drift_threshold
- None: git drift detection disabled
- Some(n): drift > n commits → trust penalty
- Never 0: use None to disable

#### conditional_exclude
- condition = "status_terminal": uses DEFAULT status (not frontmatter status)
- Applied at scan time, before parsing

#### id_rules order
- First match wins: order matters
- Set id_rules_sort_by_specificity = true for auto-sort
- OR manually order from most-specific to least-specific glob
```

---

## 전체 작업 계획 (우선순위 + 예상 시간)

### Phase 1: Critical (1주)
- [ ] Config hash semantic 명시화 (builder/mod.rs)
- [ ] Zero threshold 검증 (config.rs, rules, query)
- [ ] DAG cycle detection rule (rules/graph_invariants.rs)
- [ ] 테스트 작성 (all critical)
- **작업량**: ~1000줄

### Phase 2: High (2주)
- [ ] ID rules 순서 결정성 (config.rs + parser/identity.rs)
- [ ] Link pattern capture group validation (config.rs + parser/body.rs)
- [ ] Conditional exclude 명시화 (config.rs + scanner.rs)
- [ ] Kind coverage warning (config.rs)
- [ ] 테스트 작성 (all high)
- **작업량**: ~800줄

### Phase 3: Medium (2주)
- [ ] Config validation modularization (config.rs refactor)
- [ ] CLAUDE.md 문서화 강화
- [ ] Symlink 처리 audit + 테스트
- [ ] 문서화 예제 추가
- **작업량**: ~600줄

### Phase 4: AI-Native Features (3주)
- [ ] Context query 구현 (query/context.rs)
- [ ] Graph metrics 구현 (query/metrics.rs)
- [ ] CLI 명령 추가
- [ ] 테스트 및 문서
- **작업량**: ~1200줄

### Phase 5: Polish (1주)
- [ ] 전체 테스트 실행
- [ ] 성능 벤치마크
- [ ] 문서화 리뷰
- [ ] 버전 번프

**총 예상 시간**: 9주  
**총 코드 변경**: ~4400줄 (신규 + 수정)

---

## 성공 기준

### 논리적 정확성
- [ ] 모든 critical validation gap 제거
- [ ] config_hash 의미론적으로 안정
- [ ] cycle detection 규칙 작동
- [ ] Zero threshold 불가능

### 명시성 & 문서화
- [ ] 모든 비직관적 설정 문서화
- [ ] 모든 규칙 is_applicable 명시
- [ ] 모든 validator 테스트 포함

### 확장성 (AI-native)
- [ ] Context query로 LLM 프롬프트 1줄 생성 가능
- [ ] Metrics로 그래프 건강도 자동 추적
- [ ] webloom/aix-platform 통합 테스트 통과

### 코드 품질
- [ ] 69 → 99 validation tests
- [ ] config.rs 모듈화 (1500줄 → 수십 개 함수)
- [ ] 0 clippy warnings
- [ ] 100% documentation coverage (pub items)
