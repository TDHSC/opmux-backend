# Independent Administration Application Contract

Status: proposed, 2026-09-22. This specifies the minimum management product and its data boundary;
it is not a frontend implementation, framework choice, visual mockup, or development plan.

## 1. Separation and deployment boundary

The administration application has its own codebase/build/deployment. Gateway owns no HTML bundle,
frontend dependencies, page routing, or direct Supabase browser access. The integration artifact is
[openapi.json](openapi.json), its [transport semantics](api-contract.md), and
[synthetic examples](examples.json).

```mermaid
sequenceDiagram
    actor Operator
    participant Browser as Admin browser
    participant Host as Admin server adapter
    participant Gateway as Opmux management API
    Operator->>Browser: Open customer allowance
    Browser->>Host: Authenticated same-origin GET
    Host->>Host: Resolve session and permitted tenant
    Host->>Gateway: GET with server-held tenant management key
    Gateway-->>Host: Policy DTO + ETag
    Host-->>Browser: Policy DTO + ETag (no API key)
    Operator->>Browser: Review limit and reason; save
    Browser->>Host: Mutation + CSRF token + Idempotency-Key + If-Match
    Host->>Gateway: Authorized conditional mutation
    Gateway-->>Host: Committed DTO + operation ID
    Host-->>Browser: Confirmed result
```

The server adapter is part of the separate admin application. It holds the selected tenant's
management credential and resolves a human session to an allowed tenant. Its browser-facing prefix
is `/management/api`; it forwards the remainder to `/api/v1/usage`, preserving DTOs, status codes,
ETags, operation IDs, and idempotency keys. For example, `/management/api/customers` maps to
`/api/v1/usage/customers`.

The adapter validates and strips `X-CSRF-Token` before forwarding, and forwards only the contract
headers. It returns the canonical error envelope for its own failures too; a locally generated
request ID links its logs. Cookie/session establishment endpoints belong to the host authentication
contract and are not gateway endpoints.

It is an allowlisted adapter, not an arbitrary HTTP proxy. It rejects browser `X-API-Key`, tenant
override headers, upstream URLs, and unknown routes. It never exposes inference or key-provisioning
endpoints through this prefix. There is no direct browser call to the gateway management surface.

## 2. Human authentication and permissions

The host supplies human login/session establishment; selecting an identity provider is outside the
four-feature scope. A public admin deployment is incomplete until that integration exists. A local
operator deployment must still authenticate the operator and protect credentials; loopback alone is
not a browser session model.

- Session credentials use Secure, HttpOnly cookies in HTTPS deployments, suitable SameSite policy,
  origin checks, and `X-CSRF-Token` validation for mutations. Do not place management keys or
  session tokens in localStorage, sessionStorage, URL parameters, static assets, or browser logs.
- The adapter authorizes each request from server-side session/tenant membership. A selected-tenant
  dropdown is not authorization. Cross-tenant responses never share client cache keys.
- The minimum app grants full usage-administrator capability for one authorized tenant. Fine-grained
  enterprise roles are outside v1; an optional read-only host role must be enforced by the adapter,
  not only by hidden buttons. The gateway still sees management capability.
- The gateway audit records the authenticating API key. The host records the human actor and links
  its audit to the returned operation ID. Do not claim that arbitrary actor headers provide trusted
  gateway-level human attribution.
- Logout/tenant change clears fetched usage state, pending forms, and local cached DTOs. An
  in-flight mutation remains bound to its original tenant and operation key; its response cannot
  update another tenant's screen.

## 3. Screen-to-contract mapping

| Screen                | Primary data                                                                   | Actions and contract dependencies                                                                    |
| --------------------- | ------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------- |
| Overview              | GET settings, tenant balance, summary grouped by day, unacknowledged events    | Show mode and uncertainty; open customer/request detail; acknowledge events                          |
| Customers             | GET customer page; exact external-ID lookup                                    | Create customer; open detail; paging preserves filters                                               |
| Customer detail       | GET customer, customer policy/balance, customer summary, customer request page | Rename, suspend/resume, edit recurring limit, adjust current capacity                                |
| Request detail        | GET usage request, paginated attempts                                          | Explain retry/fallback sequence, selected/reported models, estimated cost and unknown holds          |
| Allowance editor      | Latest configuration DTO + ETag; current balance                               | Choose current-and-future or next-period effect, enter exact amount and reason, preview consequences |
| Warning inbox         | GET events filtered by acknowledgement/customer                                | Acknowledge; open associated account/request/attempt                                                 |
| Reconciliation detail | GET attempt + lifecycle ETag                                                   | Attest reviewed token usage or explicitly release uncertain capacity, with reason/evidence reference |
| Change history        | GET audit with optional account/customer/attempt target                        | Inspect API-key actor, reason, before/after fields, operation ID and time                            |

These can be panels/tabs in a small application; eight routes or a separate analytics product are
not required. No charts need data outside the published APIs. A usage graph displays settled
estimates separately from reserved/uncertain exposure, not a single misleading spend line.

Customer lists show identity/status first. They do not fan out one balance request for every row.
Opening a detail loads its exact balance. An optional cost column can join a bounded summary page by
customer ID, but absence from that page must display unavailable, not zero. A future batched balance
endpoint requires an explicit contract addition; database access is not a shortcut.

## 4. Display semantics

Use user-facing wording that preserves evidence boundaries:

| Transport data                          | Suggested label / behavior                                                            |
| --------------------------------------- | ------------------------------------------------------------------------------------- |
| `mode: observe`                         | Observation only; limits are not enforced                                             |
| `mode: enforce`                         | Allowance enforcement on; attempts still require valid bounds                         |
| `settled_estimate_usd`                  | Estimated usage cost                                                                  |
| `active_hold_usd`                       | Reserved for active calls                                                             |
| `pending_exposure_usd`                  | Held while usage is uncertain                                                         |
| `carryover_hold_usd`                    | Outstanding holds from earlier months                                                 |
| `available_usd`                         | Available for new reservations; actual admission may change concurrently              |
| `available_usd: null`                   | Not determinable / no configured limit, distinguished using policy and unknown fields |
| `overcommitted_usd > 0`                 | Exposure exceeds allowance; show exact shortfall                                      |
| `usage_evidence: provider_reported`     | Provider-reported tokens; price is still an estimate                                  |
| `usage_evidence: operator_attested`     | Usage entered after operator review                                                   |
| `settlement_state: released_unverified` | Capacity released; provider cost remains unknown                                      |
| `complete: false`                       | Incomplete usage evidence; totals include only known components                       |

Format USD for ordinary reading, with full nine-decimal precision available on detail/copy. Use a
decimal library/string arithmetic for totals and comparisons, never JavaScript floating-point money
calculations. A tiny positive amount must not display as an unqualified `$0.00`; show `< $0.01` with
its exact value available. The server supplies accounting results; the browser may preview an edit
but cannot determine final balance or authorization.

Display times in the operator's preferred zone while marking the allowance cycle as UTC. Filters are
converted to explicit UTC boundaries. The recurring month end is exclusive. Account summary and
detail pages display `as_of` and the selected range, so a live balance is not confused with a
historical range total.

An event acknowledgement hides/marks the warning as acknowledged, never labels its underlying
uncertainty resolved. If settlement later completes, refreshing the linked attempt shows that
change. Event payloads are a historical observation, not a current balance snapshot.

## 5. Allowance edit interaction

1. Fetch policy and balance. Keep the exact ETag with the form. Explain whether the current policy
   is absent, zero, active, or has a scheduled change.
2. Accept a nonnegative recurring USD amount, effect choice, and required reason. Current-period
   adjustments use a separate signed amount form to avoid confusing one-time capacity with recurring
   allowance. Never offer a reset-consumption button.
3. Preview the new current/future limit. A reduction below known exposure explicitly shows that it
   blocks new attempts and does not revoke already reserved or dispatched work. Unknown amounts
   prevent a definitive preview. A next-period edit clearly leaves the current month unchanged.
4. Generate one idempotency key for the intended mutation and keep it stable through retries.
   Disable duplicate submission while its outcome is unknown. Send If-Match and the audit reason.
5. On confirmed success, show the operation ID and refetch policy/balance/events. Do not
   optimistically change the authoritative balance or mark a grant complete before the response is
   resolved.
6. On 412, refetch and show the intervening change. Keep the user's typed amount/reason separately,
   but require review before sending a new edit with a new key and ETag.
7. On a network error or 503/504, display an unknown outcome and allow retry of the exact same
   operation. Do not create a new operation key merely because a timeout occurred.

Mode activation uses an explicit review of tenant/customer policies and unresolved unknown exposure.
Enforce activation is a consequential setting change, not an automatic side effect of saving a
customer limit. Releasing uncertain exposure uses a separate confirmation explaining that provider
consumption may still exist, then submits the explicit `release_unverified` action. Ordinary
inspection/refresh and harmless navigation need no confirmation.

## 6. UI state contract

| State                           | Required behavior                                                                                         |
| ------------------------------- | --------------------------------------------------------------------------------------------------------- |
| Loading                         | Keep labels/layout stable; never render placeholder zeros as recorded spending                            |
| Empty customer list             | Explain that no customers are registered and expose creation                                              |
| Empty usage range               | Report no recorded activity for that filter, not no provider bill                                         |
| Missing allowance               | Show unconfigured policy and setup action; do not display unlimited authorization                         |
| Partial/unknown usage           | Preserve known values, show uncertain counts/holds, link to attempts                                      |
| Refresh failure with prior data | Keep prior values visibly stale with their timestamp; do not silently treat them as current               |
| 401                             | Reauthenticate the host session or diagnose its server-held credential; never ask for a key in a page URL |
| 403                             | Explain missing permission or customer suspension from the stable code                                    |
| 404                             | Generic unavailable resource state without tenant existence hints                                         |
| 409                             | Branch on duplicate/in-progress/policy/evidence code; do not treat every conflict as retryable            |
| 412 / 428                       | Reload the editing precondition; preserve user input for review                                           |
| 429 allowance                   | Explain insufficient reservation capacity; no automatic generation retry loop                             |
| Backend unavailable             | Show a retryable read state; for mutations preserve the idempotency key and unknown outcome               |

Poll active overview/detail data no more often than every 15 seconds while the page is visible;
pause hidden tabs and back off on repeated failures. This is a client default, not a backend SLA.
Requests have cancellation and stale-response protection when filters/tenants change. Manual refresh
starts a new cursor/snapshot; it does not combine incompatible pages.

Keyboard navigation, associated field labels, accessible error text, visible focus, and non-color
status indications apply to every form/state. Do not make charts the only way to read costs or
require hovering to discover an uncertain balance.

## 7. Independent consumer fixture expectations

[examples.json](examples.json) contains schema-named synthetic DTOs, including a successful fallback
with an unknown primary attempt and a balance carrying exposure from a previous month. They can
drive a frontend mock transport without a running gateway or provider credentials.

Consumers must also represent empty pages, null/no-policy balance, suspended customer, exhausted and
overcommitted capacity, unbounded uncertainty, stale ETags, duplicate submission, and lost mutation
responses. These are contract acceptance states, not a prescribed implementation/test task list.

The contract supplies all financial data and management operations required by this minimum app.
Human-login pages, user invites, payment pages, invoices, prompt debugging, model playgrounds,
workflow builders, and provider-key administration are outside its scope.
