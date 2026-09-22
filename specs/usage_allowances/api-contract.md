# HTTP Contract Semantics

Status: proposed contract version `0.1.0`, 2026-09-22. The endpoints below are not implemented.
[openapi.json](openapi.json) defines wire schemas; [design.md](design.md) defines accounting
semantics. The document is an OpenAPI 3.1.1 design artifact, not generated server code.

## 1. Common transport rules

- HTTPS outside owned loopback development. JSON request/response bodies use UTF-8.
- `/api/v1/usage/*` requires the existing management `X-API-Key` capability. `/api/v1/route`
  requires inference capability. Tenant identity is never a path/body/query selector.
- The server rejects unknown write properties, unknown/duplicate query parameters, unsupported
  content types, malformed identifiers, and whitespace-only display names/reasons. Optional means
  omitted, not null, unless the schema explicitly permits null.
- Missing/invalid credentials return 401; wrong key capability returns 403. Unknown and cross-tenant
  resource IDs both return 404. A cursor must be signed and bound to the authenticated tenant,
  operation, filters, range, sort, page size, and fixed membership upper bound.
- Monetary fields use exact decimal strings with nine places. Input never accepts exponent notation,
  currency symbols, separators, NaN, or float values. Signed capacity adjustments must be nonzero.
- Counts in new APIs are integer strings; existing generation usage counts keep their current JSON
  number representation. Customer/task identifiers are opaque ASCII labels or UUIDs, not emails.
- Every response includes `X-Request-ID`; all usage/management responses use
  `Cache-Control: no-store`. Never place API keys in URLs. `X-Opmux-Operation-ID` identifies a
  committed management mutation.
- Mutable configuration GETs return strong ETags. Writes require exactly one matching `If-Match`;
  wildcard preconditions are not accepted. Account ETags include the policy revision and effective
  UTC month, so scheduled activation/month rollover may require a refresh. Ordinary consumption does
  not change policy ETags. Attempt ETags use its lifecycle revision.
- Mutations require `Idempotency-Key`, including acknowledgement. Acknowledgement uses the fixed
  audit reason `Acknowledged warning`; financial/configuration mutations require a supplied reason.
  A key is tenant/operation/target scoped. Replay returns the original safe result plus
  `Idempotency-Replayed: true`. Keep the same key, body, and precondition when retrying an ambiguous
  transport result. Editing the request requires a fresh key. See the backend design for seven-day
  management and 90-day inference windows.
- New response properties may be added in compatible minor versions; consumers ignore unknown
  response properties. New unknown enum values render an explicit unsupported state, never a guessed
  financial state. Removing/renaming fields or changing amount meanings requires a versioned
  contract.
- Request schemas are strict. The closed response schemas in this contract describe version 0.1.0
  fixtures; they do not instruct clients to reject harmless additional fields from a newer version.

## 2. Resource operations

All paths below start with `/api/v1/usage`.

| Method and path                                       | Result                             | Write requirements / key semantics                                                 |
| ----------------------------------------------------- | ---------------------------------- | ---------------------------------------------------------------------------------- |
| `GET /settings`                                       | Mode, revision, update time        | ETag; no mutation                                                                  |
| `PUT /settings`                                       | Updated mode                       | Idempotency-Key, If-Match, reason; enforce prerequisites checked atomically        |
| `GET /customers`                                      | Cursor page of customers           | Optional exact external_id/status filter                                           |
| `POST /customers`                                     | 201 customer                       | Idempotency-Key, reason; duplicate external ID is 409                              |
| `GET /customers/{customer_id}`                        | Customer                           | ETag; same tenant only                                                             |
| `PATCH /customers/{customer_id}`                      | Updated name/status                | Idempotency-Key, If-Match, reason; external ID immutable                           |
| `GET /allowance`                                      | Tenant policy                      | ETag; unconfigured policy is a 200 typed resource, not infinite credit             |
| `PUT /allowance`                                      | Tenant policy                      | Idempotency-Key, If-Match, reason; current-and-future or next-period effect        |
| `GET /customers/{customer_id}/allowance`              | Customer policy                    | Same semantics as tenant policy                                                    |
| `PUT /customers/{customer_id}/allowance`              | Customer policy                    | Same semantics as tenant policy                                                    |
| `POST /allowance/adjustments`                         | 201 capacity adjustment            | Idempotency-Key, policy If-Match, reason; current period only                      |
| `POST /customers/{customer_id}/allowance/adjustments` | 201 customer capacity adjustment   | Same semantics as tenant adjustment                                                |
| `GET /balance`                                        | Current tenant balance             | Informational snapshot, not admission authorization                                |
| `GET /customers/{customer_id}/balance`                | Current customer balance           | Includes current and older outstanding holds                                       |
| `GET /summary`                                        | Totals plus paginated groups       | Required from/to/group_by; optional customer/workflow/task/target filters          |
| `GET /requests`                                       | Request page                       | Required from/to; optional attribution, target, execution/accounting-state filters |
| `GET /requests/{usage_request_id}`                    | Request detail and totals          | No input/output text                                                               |
| `GET /requests/{usage_request_id}/attempts`           | Attempt page                       | Ascending ordinal; every retry/fallback separately visible                         |
| `GET /attempts/{attempt_id}`                          | Attempt and price snapshot         | Lifecycle ETag for a resolution                                                    |
| `POST /attempts/{attempt_id}/resolutions`             | 201 resolution and updated attempt | Idempotency-Key, If-Match, reason, evidence reference; pending attempts only       |
| `GET /events`                                         | Persistent warning page            | Required from/to; optional customer/acknowledged filter                            |
| `POST /events/{event_id}/acknowledgement`             | Event with acknowledgement         | Idempotency-Key; no body; repeated acknowledgement preserves original actor/time   |
| `GET /audit`                                          | Management operation page          | Required from/to; optional target_id filter                                        |

Customer creation provisions its empty allowance account atomically. Tenant usage activation/
provisioning similarly creates its settings and account. GET never creates a financial row. Period
balance reads can represent an unmaterialized new month as a virtual zero-settled period with the
configured base and actual carry-over holds; the next mutation creates it under the account lock.

The first policy PUT uses the ETag returned by `configured:false` GET. `next_period` on an
unconfigured account leaves the current month unconfigured, so enforcement cannot start yet.
`current_and_future` replaces the recurring base and clears a scheduled future change; `next_period`
replaces only the pending base. Current-period adjustments remain intact in both cases. Adjustment
mutations increment the policy's management revision and require the UI to refetch policy/balance.

## 3. Inference extension and compatibility

The existing `/api/v1/route` gains only an optional top-level `attribution` object and an optional
`Idempotency-Key` in observation mode. Both customer attribution and the key are required after an
explicit transition to enforcement. Required `metadata`, routing controls, supported parameters,
non-streaming response fields, and upstream error semantics otherwise remain unchanged.

```http
POST /api/v1/route
X-API-Key: <inference credential held by the SaaS backend>
Idempotency-Key: generation_run42_step1
Content-Type: application/json
```

```json
{
  "prompt": "Extract the invoice fields.",
  "metadata": {},
  "parameters": { "max_tokens": 128 },
  "attribution": {
    "customer_id": "00000000-0000-4000-8000-000000000001",
    "workflow_id": "extract_invoice",
    "task_id": "run_42"
  }
}
```

After durable admission, success and error responses include `X-Opmux-Usage-Request-ID`. An HTTP
failure before admission has no usage ID and no financial reservation. A later fallback reservation
denial can have a usage ID and earlier attempt consumption. The response's existing `cost` field
continues to describe only the successful response; total request exposure is obtained from usage
APIs. Neither a business task ID nor X-Request-ID suppresses duplicate execution.

A repeated inference Idempotency-Key returns 409 and the original usage ID, even if its original
generation succeeded. No generated answer is stored for replay. Callers must distinguish an
intentional new generation from checking the status of an ambiguous prior request. A management
client can inspect that usage ID; inference credentials cannot fetch tenant-wide accounting data.

## 4. Numeric and reporting semantics

Example balance fields:

```json
{
  "effective_limit_usd": "100.000000000",
  "settled_estimate_usd": "62.250000000",
  "active_hold_usd": "1.000000000",
  "pending_exposure_usd": "2.500000000",
  "carryover_hold_usd": "0.750000000",
  "total_held_usd": "4.250000000",
  "available_usd": "33.500000000",
  "overcommitted_usd": "0.000000000",
  "unbounded_unknown_attempt_count": "0",
  "complete": false
}
```

This is an excerpt, not a complete Balance DTO. Full schema-valid payloads are in
[examples.json](examples.json). The $2.50 pending component is a retained bound, not known spending.
`active_hold_usd` and `pending_exposure_usd` cover current-period bounded holds; carry-over covers
all older bounded holds. Their sum is `total_held_usd`. Settled amounts exclude unresolved holds.

When an unbounded unknown exists, the bounded components still report their known subtotals, but
`available_usd` and `overcommitted_usd` are null and `complete` is false. Null availability is not
zero and not unlimited. Without a configured policy, limit/availability fields are also null.
Observation mode may show negative-capacity consequences via positive `overcommitted_usd`; it does
not claim enforcement. An unverified release leaves the usage amount unknown even though its hold
has ended. The detailed attempt and historical totals retain that distinction.

Summary ranges are half-open UTC `[from,to)`, maximum 31 days. Required `group_by` selects one of
day/customer/workflow/target/task. Filtering on customer IDs checks tenant ownership. Day keys are
`YYYY-MM-DD`; other group keys are opaque identifiers; unassigned attribution uses a null key.
Totals represent the entire filter, not just one group page. Request counts are distinct operations
having attempts in the range; a request can appear in multiple target/day groups. Do not sum group
request counts to derive the overall count. Monetary/attempt totals are additive for disjoint
groups.

Request-list time filters refer to request admission, while summary filters refer to attempt
admission. A target filter on a request list selects any matching attempt but returns the complete
request totals; on summaries it selects only matching attempts. The UI must not label those results
as the same metric. Known token totals exclude unknown attempts and are visibly incomplete when
`complete:false`.

Lists use limit 1–100, default 25. Cursors expire after 15 minutes and bind the filter/page-size
tuple. Request/event/audit/customer ordering is descending timestamp then ID; attempts order by
ascending ordinal; summary groups by stable key with null first. A cursor fixes list membership's
upper boundary, not the accounting state of each record. Late evidence may change values on later
pages. Refresh resets the cursor. Expired/invalid cursors return 400 `INVALID_CURSOR`.

## 5. Error contract and retry behavior

All errors keep the existing shape; clients branch on `code`, never the English message.

```json
{
  "error": {
    "code": "ALLOWANCE_EXCEEDED",
    "message": "Insufficient allowance for the next provider attempt.",
    "request_id": "example-request-2"
  }
}
```

| HTTP      | Code                                             | Meaning / client action                                                                                   |
| --------- | ------------------------------------------------ | --------------------------------------------------------------------------------------------------------- |
| 400       | `INVALID_REQUEST`, `INVALID_CURSOR`              | Correct inputs; do not blind-retry                                                                        |
| 400       | `CUSTOMER_REQUIRED`, `IDEMPOTENCY_REQUIRED`      | Enforced generation lacks required attribution/key                                                        |
| 401 / 403 | Existing `UNAUTHORIZED` / `FORBIDDEN`            | Restore authorized credentials; never retry with a different tenant selector                              |
| 403       | `CUSTOMER_SUSPENDED`                             | Stop; management decision required                                                                        |
| 404       | `NOT_FOUND`                                      | Unknown or out-of-tenant object; indistinguishable                                                        |
| 409       | `CUSTOMER_EXISTS`                                | Query the existing tenant-local external ID; do not create another customer                               |
| 409       | `POLICY_NOT_CONFIGURED`, `ENFORCEMENT_NOT_READY` | Set missing policy or resolve bound/unknown prerequisite                                                  |
| 409       | `IDEMPOTENCY_CONFLICT`                           | Same key with different logical input; investigate rather than replacing key automatically                |
| 409       | `OPERATION_IN_PROGRESS`                          | Retry the same management operation after bounded Retry-After                                             |
| 409       | `INFERENCE_ALREADY_SUBMITTED`                    | Inspect original usage ID; do not automatically start another generation                                  |
| 409       | `ATTEMPT_NOT_PENDING`, `EVIDENCE_CONFLICT`       | Refresh attempt; a competing settlement/resolution may have won                                           |
| 412       | `PRECONDITION_FAILED`                            | Reload configuration and ask the operator to review their intended edit                                   |
| 428       | `PRECONDITION_REQUIRED`                          | Fetch ETag, then submit a conditional mutation                                                            |
| 429       | `ALLOWANCE_EXCEEDED`                             | Stop generation; inspect balance/policy. No automatic retry-at-reset promise because holds can carry over |
| 429       | Existing `OVERLOADED`, `UPSTREAM_RATE_LIMIT`     | Preserve their existing distinct meanings; allowance limits are not provider throttling                   |
| 503       | `ACCOUNTING_UNAVAILABLE`, `ACCOUNTING_BUSY`      | Persist the operation key and retry status/replay safely; do not assume rollback or free execution        |
| 503       | `METERING_UNAVAILABLE`                           | Unsupported/missing verified bound; operator configuration required                                       |
| 504       | Existing `DEADLINE_EXCEEDED`                     | Outcome may be uncertain; inspect durable usage/operation status                                          |

Other existing ingress/adapter errors remain documented in
[API_REFERENCE.md](../../docs/API_REFERENCE.md). A provider error never authorizes releasing a
possibly charged hold without evidence. A failed management response is not proof its transaction
rolled back. Idempotent replay is the resolution path, with a new HTTP correlation ID and the same
durable operation ID.

## 6. Contract ownership and conformance

The backend owns accounting semantics and publishes OpenAPI plus synthetic examples. The admin
application owns presentation and its human authentication boundary. Neither side shares database
row structs or derives the other's behavior from an ORM. Generated clients/types may consume this
contract later; this design does not require a specific frontend framework or generator.

Schema validation can prove example shape, required fields, references, and operation completeness.
It cannot prove tenant isolation, transaction ordering, cost bounds, recovery behavior, or deployed
compatibility. The acceptance properties in requirements/design remain necessary behavioral checks
when implementation exists.
