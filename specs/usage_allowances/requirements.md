# Requirements and Evidence

Status: proposed design, 2026-09-22. Scope: the four capabilities described in
[README.md](README.md).

## Product outcome

An AI SaaS backend can identify the customer and task responsible for model consumption, inspect the
evidence behind estimated cost, assign a monthly allowance, and prevent new provider attempts that
cannot be covered by that allowance. An administrator can inspect and change this state through a
separate application consuming the same published management API.

The buyer-facing questions are: who used it, which task caused it, what is known about its cost, how
much capacity remains, and why was a request stopped?

## Research and applicability

Sources were revisited on 2026-09-22. External material supports the problem or an engineering
practice; it does not establish Opmux demand, willingness to pay, uniqueness, or a savings claim.
The specific schema, defaults, and algorithms below are Opmux design decisions.

| Evidence                                                                                                                                                  | What it supports                                                                                              | Design consequence                                                                              | Boundary                                                                          |
| --------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| [Wordware customer case, Helicone](https://www.helicone.ai/customers/wordware), May 2025                                                                  | Customers asked for the cost of particular workflow runs; the team previously assembled reports manually      | First-class customer/workflow/task attribution and request-to-attempt drill-down                | Supplier-published case; no independently verified Opmux purchase or ROI evidence |
| [LangChain's internal gateway experience](https://www.langchain.com/blog/how-we-made-coding-agent-spend-predictable), June 2026                           | Spend control requires attribution, price maintenance, early warning, and auditable limit changes             | Versioned prices, threshold events, and reason-bearing management mutations                     | Internal vendor practice, not an external customer purchase study                 |
| [LiteLLM customer budgets](https://docs.litellm.ai/docs/proxy/customers)                                                                                  | Customer-level tracking and budgets are an existing product category                                          | Deliver a coherent embedded usage workflow; do not claim budget fields are a unique advantage   | Competitor capability is not evidence of a market gap                             |
| [Stripe meter event guidance](https://docs.stripe.com/billing/subscriptions/usage-based/recording-usage-api)                                              | Metering events require explicit identity and duplicate handling; downstream summaries can be asynchronous    | Stable local event identities; local transactional enforcement independent of a billing service | No Stripe integration or invoice semantics in v1                                  |
| [PostgreSQL row locks](https://www.postgresql.org/docs/17/explicit-locking.html) and [isolation](https://www.postgresql.org/docs/17/transaction-iso.html) | Row locking coordinates concurrent writers; ordinary reads alone do not serialize a read/check/write sequence | Short transactions lock the same allowance accounts in a fixed order                            | The Opmux invariant depends on every writer following that protocol               |
| [PostgreSQL exact numeric types](https://www.postgresql.org/docs/17/datatype-numeric.html)                                                                | Exact numeric arithmetic is available for quantities where binary floating-point is unsuitable                | Integer nano-USD accounting and decimal-string DTOs                                             | No claim that configured prices equal provider invoices                           |
| [OWASP object authorization](https://owasp.org/API-Security/editions/2023/en/0xa1-broken-object-level-authorization/)                                     | Authorization must be checked for each accessed object                                                        | Authenticated tenant predicates and composite foreign keys throughout                           | UUIDs are identifiers, not authorization                                          |
| [OWASP browser storage guidance](https://cheatsheetseries.owasp.org/cheatsheets/HTML5_Security_Cheat_Sheet.html)                                          | Script-readable storage is unsuitable for sensitive credentials/session identifiers                           | A server-side administration adapter holds management credentials                               | A static frontend alone is not a safe management deployment                       |
| [OpenAPI 3.1.1](https://spec.openapis.org/oas/v3.1.1.html) and [HTTP conditional requests](https://www.rfc-editor.org/rfc/rfc9110.html#name-if-match)     | Machine-readable HTTP contracts and conditional writes support independent clients                            | OpenAPI contract, explicit DTOs, ETag/If-Match for editable configuration                       | An OpenAPI document is not implementation or conformance proof                    |

## Verified implementation baseline

The current [README](../../README.md) and source were inspected for this proposal:

- [AuthContext](../../gateway/src/features/auth/models.rs) contains authenticated `client_id`,
  `key_id`, and management/inference kind. One existing client is one Opmux tenant.
- [Private auth schema](../../supabase/migrations/20260919194500_create_opmux_private_auth.sql) uses
  PostgreSQL roles and is not exposed through the Supabase Data API.
- [Ingress parsing](../../gateway/src/features/ingress/validate.rs) rejects unknown controls.
  `metadata` is required but opaque and must remain unlogged and unpersisted.
- [Ingress service](../../gateway/src/features/ingress/service.rs) selects configured routes. Its
  business method does not currently receive tenant identity; this proposal requires an explicit
  authenticated execution context across that boundary.
- [ExecutorService](../../gateway/src/features/executor/service.rs) owns retries, fallback,
  deadlines, and circuit handling. [AttemptBudget](../../gateway/src/features/executor/budget.rs)
  limits the number of actual attempts; it is not a monetary allowance.
- [Pricing](../../gateway/src/features/executor/pricing.rs) computes an `f64` estimate for a
  successful response only. It is unsuitable as an authoritative exact ledger input.
- [Attempt guards](../../gateway/src/features/executor/attempt.rs) currently record metrics; an
  in-memory drop guard is not durable settlement or crash recovery.
- There is no usage ledger, customer allowance model, or administration application in this
  checkout. Live-provider and hosted verification remain deferred; this design changes neither.

## Actors and trust

| Actor                               | Allowed actions                                                                                                            |
| ----------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| Opmux tenant's trusted SaaS backend | Send inference requests with its inference key and tenant-local customer/task attribution                                  |
| Tenant management client            | Register customers; query usage; configure and adjust allowances; acknowledge events; resolve uncertain holds              |
| Separate administration server      | Authenticate its human operator, authorize tenant access, and call the management API with a server-held tenant credential |
| End customer/browser                | Receive a filtered view through the SaaS/backend adapter; never receive a tenant management key                            |
| Opmux operator                      | Provision tenants, maintain price/bound profiles, operate recovery, and investigate infrastructure incidents               |

Customer attribution is not end-user authentication. A tenant-wide inference key is a trusted
backend credential. A browser supplied with that key could choose any customer in that tenant;
browser-direct generation and customer-bound key delegation are outside v1.

## Functional requirements

| ID  | Required outcome and observable behavior                                                                                                                                                              |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| U1  | Every accepted generation has a server-generated usage request ID and authenticated tenant/key ownership. Caller request IDs are correlation only.                                                    |
| U2  | Every actual provider attempt has a unique identity, request ordinal, selected target, price snapshot, exposure bound, outcome, and usage evidence. Skipped circuits/targets are not billed attempts. |
| U3  | Customer, workflow, and task filters explain a request across retries/fallbacks. A task ID groups multiple requests but does not deduplicate them.                                                    |
| U4  | Exact settled estimates, active reservations, unknown exposure, and manually released uncertainty remain distinguishable. Unknown consumption is never silently reported as zero.                     |
| U5  | Historic events are immutable; corrections append evidence and adjustment entries. Totals can be rebuilt and checked against transactional balance projections.                                       |
| A1  | In enforcement mode, every generation requires a registered active customer and finite tenant/customer allowances. Missing policies or unverifiable bounds stop dispatch.                             |
| A2  | Before each provider attempt, all applicable accounts reserve sufficient capacity atomically. Concurrent processes cannot spend the same available capacity.                                          |
| A3  | Finalization releases only unused exposure; ambiguous dispatch/usage retains a hold. Retry/fallback requires its own reservation under the same attempt/deadline policy.                              |
| A4  | A new UTC calendar month creates a new period. Unresolved old holds continue to constrain admission; a calendar boundary does not erase uncertainty.                                                  |
| A5  | Management can edit recurring limits, grant/reduce current-period capacity, suspend a customer, and inspect audit history. Changes require a reason and conflict protection.                          |
| Q1  | APIs return customer balances, period totals, filtered request/attempt records, and events without exposing prompts, completions, credentials, or other tenants.                                      |
| Q2  | Decimal strings, explicit currency/evidence fields, bounded filters, cursor pagination, timestamps, and sanitized stable errors define the transport.                                                 |
| Q3  | Admission uses transactionally current local data. A UI cache, dashboard query, or external billing summary never authorizes a call.                                                                  |
| M1  | The minimum management application is independently deployable and depends only on the published HTTP contract, not database tables or Rust internals.                                                |
| M2  | Its screens cover overview, customers, request detail, allowance editing, and persistent warnings; empty/loading/stale/error/conflict states are specified.                                           |
| M3  | Threshold warnings and allowance edits have durable event/audit records. Network retries cannot duplicate a grant or resolution.                                                                      |

## Deliberate v1 defaults

These are proposed product defaults, not facts established by research:

- USD only; exact accounting at nine decimal places. Limits are cost-equivalent allowances using
  configured provider tariffs, not retail credits, tokens, prepaid cash, or subscription
  entitlements.
- One tenant allowance plus one allowance per registered customer; workflow/task attribution is
  informational. No arbitrary budget hierarchy or per-workflow hard cap.
- UTC calendar months, no rollover or prorating. Recurring base limits plus signed, current-period
  capacity adjustments. No manual reset/delete of consumption.
- Default mode `observe`; explicit management transition to `enforce`. Observation still records
  durable usage and uncertainty but does not claim a spending cap.
- No automatic cheaper-model substitution when capacity is insufficient.
- Management reads are tenant-wide; the SaaS backend applies its own end-customer authorization.
- In-app warnings and a pollable event API; email, Slack, outbound webhooks, and delivery channels
  are outside this design's feature scope.
- Existing non-streaming, text-only generation remains the execution surface. Protocol expansion,
  streaming, tool calls, additional vendors, memory, rewriting, and intelligent routing are
  separate.
- No payment collection, invoicing, taxes, provider invoice reconciliation integration, or new human
  identity provider. Human authentication is a prerequisite supplied by the separate admin host.

## Acceptance properties, not a development plan

The design is satisfied only if the following can be demonstrated when it is implemented:

- Cross-tenant IDs never expose an object or affect another tenant's balance.
- With $1.00 available and two concurrent $0.60 reservations, at most one dispatch is admitted.
- A $0.60 hold settled at $0.20 leaves $0.80 available; duplicate settlement changes nothing.
- An uncertain $0.60 attempt followed by a $0.20 fallback consumes $0.20 and retains $0.60 until
  evidence or an explicit audited release resolves it.
- A process crash after durable dispatch intent never causes automatic redispatch or silent release.
- A month boundary preserves old holds and period attribution; late evidence cannot be moved into
  the new month to make old reporting convenient.
- Stale allowance edits fail instead of overwriting a newer administrator's decision.
- Query totals, detail views, and allowance balances expose their distinct meanings consistently.
- A separate frontend can implement every specified screen using the contract and sample payloads.
