# Enhanced Observability - Specification

## Overview

This specification describes the implementation of enhanced observability features for the Gateway
Service, including structured logging with correlation IDs, Prometheus metrics collection, and
enhanced health check endpoints.

**Status**: Historical specification. Correlation IDs, structured logs, Prometheus HTTP and bounded
execution metrics, `/health` liveness, and `/ready` dependency probes **are shipped**. Selectable
`HEALTH_CHECK_MODE`, 30-second health cache, degraded "at least one vendor" readiness, and
distributed tracing export are **not shipped**. This is not a hosted/production-readiness claim.
OpenAI checks are **SIMULATED ONLY**.

**Related Requirement**: Requirement 5 - Monitoring and Observability (from
`specs/gateway_service/requirements.md`)

**Implementation Phase**: Phase 1 (API MVP) — see [README.md](../../README.md) for current behavior.

## Documents

- **[design.md](./design.md)** - Technical design document with architecture, technology stack, and
  implementation details
- **[tasks.md](./tasks.md)** - Detailed task breakdown with 10 main tasks and 40+ subtasks

## Scope

### Phase 1 (Current Spec)

✅ **Structured Logging + Correlation IDs**

- Dual-ID system (system request_id + client correlation_id)
- HTTP header propagation (X-Request-ID, X-Correlation-ID)
- Tracing spans across all layers (Handler/Service/Repository)
- JSON-formatted structured logs

✅ **Prometheus Metrics**

- Automatic HTTP metrics collection (latency, throughput, error rates)
- `/metrics` endpoint for Prometheus scraping
- Using `axum-prometheus` for seamless integration
- Bounded execution metrics: attempts/outcomes, retries, fallback, circuit state/transitions,
  deadlines, local overload, and successful usage. Labels are route templates, configured target
  IDs, and finite outcome classes. `/metrics` is an internal scrape surface (network restriction,
  not a metrics auth system).

✅ **Health and readiness (shipped behavior)**

- `/health` is process liveness only
- `/ready` requires authentication-database schema/access, upstream `GET /models`, and at least one
  usable default-route target
- Success-only cache: `HEALTH_CHECK_CACHE_TTL_SECS` default **5 seconds**; failures are never cached
- Probe timeout: `HEALTH_CHECK_TIMEOUT` default 2 seconds
- No selectable `HEALTH_CHECK_MODE`, no degraded "one vendor healthy" shortcut, no 30s TTL

The hybrid `config` vs `connectivity` mode below is **historical design**, not shipped.

### Phase 2 (Future)

⏸️ **Distributed Tracing**

- OpenTelemetry integration
- Cross-service trace propagation
- Jaeger/Zipkin integration

⏸️ **Custom Business Metrics**

- Remaining items such as cache-hit series and billing-grade cost totals are not part of this MVP.
  Attempt/retry/fallback/usage counters are delivered.

⏸️ **Advanced Monitoring**

- Grafana dashboard templates
- Prometheus alert rules
- Log aggregation (Loki, Elasticsearch)

## Key Design Decisions

### 1. Dual Correlation ID System

**Decision**: System always generates its own Request ID, while preserving client-provided
Correlation ID

**Rationale**:

- ✅ System control and security (avoid malicious/duplicate IDs)
- ✅ Cross-system tracing (preserve client context)
- ✅ Flexible debugging (dual-direction lookup)
- ✅ Fail-safe design (middleware never blocks requests)

**Error Handling**:

- UUID generation failure → Timestamp fallback
- Invalid client ID → Ignore and log warning
- Malicious long IDs → Reject (max 256 chars)
- Header creation failure → Log error, continue without header

### 2. Technology Stack

| Component          | Library                          | Rationale                                         |
| ------------------ | -------------------------------- | ------------------------------------------------- |
| Structured Logging | `tracing` + `tracing-subscriber` | ✅ Already in use, industry standard              |
| HTTP Tracing       | `tower-http`                     | ✅ Official Axum middleware, seamless integration |
| Correlation ID     | `uuid`                           | ✅ Standard UUID generation, lightweight          |
| Metrics            | `axum-prometheus`                | ✅ High-level wrapper, automatic HTTP metrics     |

### 3. Architecture Pattern

**Pattern**: Middleware + Core (Cross-Cutting Concern)

**Structure**:

```
core/          - Tracing/metrics initialization
middleware/    - Correlation ID, tracing, metrics middleware
features/      - Business logic with tracing spans
```

**Rationale**:

- ✅ Follows 3-layer architecture principles
- ✅ Observability as cross-cutting concern
- ✅ Minimal impact on business logic

**Context Access Strategy**:

- Default: Automatic propagation via tracing spans
- Explicit: Pass RequestContext as parameter when needed (e.g., external API headers)

**Avoiding Duplication**:

- ✅ Add `request_id` to `#[tracing::instrument]` fields **only in Handler layer (root span)**
- ❌ Never add `request_id` in Service or Repository layers (child spans)
- ✅ Child spans automatically inherit `request_id` from parent
- ✅ Keeps logs concise and avoids redundancy

### 4. Production Performance Optimization

**Decision**: Configurable logging verbosity via environment variables

**Configuration**:

- `LOG_VERBOSE_DEBUG=false` (production) - Disable expensive options
- `LOG_VERBOSE_DEBUG=true` (development) - Enable line numbers, thread IDs

**Performance Impact**:

- Line numbers: ~10-20% overhead
- Thread IDs: ~5-10% overhead
- Production default: Both disabled

### 5. Health Check Strategy (Historical Hybrid Approach)

**Shipped:** `/health` liveness; `/ready` always probes database access, `/models` reachability, and
usable default-route targets. Success cache default 5 seconds. Failures uncached. No
`HEALTH_CHECK_MODE`.

**Historical design (not shipped):** configurable `config` vs `connectivity` modes, 30s TTL, and
degraded "at least 1 vendor healthy" readiness. Do not configure `HEALTH_CHECK_MODE`; the binary
does not implement it.

## Implementation Timeline

**Estimated Time**: 2.5-3 days

**Breakdown**:

- Day 1: Core infrastructure + Correlation middleware
- Day 2: Tracing spans + Prometheus metrics
- Day 2.5: Health checks (hybrid approach with cache) + Vendor health check
- Day 3: Integration + Testing + Documentation

**Note**: Hybrid health check adds ~0.5 day compared to config-only approach, but provides true
production readiness.

## Success Criteria

### Functional

- ✅ Every request has unique request_id
- ✅ Client correlation IDs preserved and echoed
- ✅ Structured JSON logs with correlation IDs
- ✅ Prometheus metrics collected and exposed
- ✅ Health check endpoints operational

### Non-Functional

- ✅ < 1ms latency for correlation ID generation
- ✅ < 5ms latency for metrics collection
- ✅ All tests pass (unit + integration)
- ✅ Code quality (clippy, fmt)

## Next Steps

1. Keep Phase 2 items as separate follow-up scope

## Related Documents

- `specs/gateway_service/requirements.md` - Original requirements (Requirement 5)
- `specs/gateway_service/design.md` - Gateway service architecture
- `specs/gateway_service/tasks.md` - Gateway service implementation plan
- `docs/rules/architecture.md` - Architecture and module rules

## Design Improvements (Based on Review)

### ✅ Addressed Concerns

1. **Middleware Error Handling** ⭐
   - Fail-safe design: Middleware never blocks requests
   - UUID generation fallback (timestamp)
   - Client ID validation (length, encoding)
   - Best-effort header creation

2. **Context Access in Business Logic** ⭐
   - Hybrid approach: Automatic propagation + explicit access
   - Default: Use tracing spans (minimal changes)
   - Explicit: Pass RequestContext when needed (external APIs)
   - **Anti-pattern**: Avoid adding `request_id` to child span fields (causes duplication)

3. **Health Check Granularity**
   - Historical hybrid `HEALTH_CHECK_MODE` was **not implemented**
   - Shipped `/ready` always checks database access, `/models`, and usable default-route targets
   - Success-only cache default 5 seconds; failures never cached
   - Timeout protection (2s)
   - Not a hosted/production-readiness certification

4. **Production Performance** ⭐
   - Configurable logging verbosity (`LOG_VERBOSE_DEBUG`)
   - Expensive options disabled by default
   - ~10-20% performance improvement in production

## Questions or Feedback

Please review the design and tasks documents and provide feedback on:

1. **Dual Correlation ID Strategy** - Is the dual-ID system (request_id + client_correlation_id)
   acceptable?
2. **Middleware Error Handling** - Is the fail-safe design sufficient?
3. **Context Access Strategy** - Hybrid approach (automatic + explicit) acceptable?
4. **Avoiding Duplication** - Is the "root span only" rule clear and sufficient?
5. **Health Check Strategy** - Historical question. Shipped behavior has no `HEALTH_CHECK_MODE`.
6. **Health Check Cache** - Shipped default is 5 seconds, success-only.
7. **Performance Optimization** - Is configurable logging verbosity sufficient?
8. **Technology Stack** - Are the chosen libraries appropriate?
9. **Implementation Scope** - Is Phase 1 scope reasonable (includes connectivity check)?
10. **Task Breakdown** - Are tasks sufficiently detailed?
11. **Timeline** - Is 2.5-3 days realistic?

---

**Status**: Historical spec with shipped correlation/metrics/health subset documented in README
