# Customer Usage and Allowance Management v1

Status: proposed design, 2026-09-22. Nothing in this directory is shipped behavior.

This design covers four connected capabilities: attributable usage records, customer allowance
enforcement, usage/balance APIs, and a separately deployable minimum administration application. It
contains no implementation schedule, task list, or development milestones.

Read the documents in this order:

1. [Requirements and evidence](requirements.md): product outcomes, researched practices, scope, and
   explicit defaults.
2. [Backend design](design.md): identity, accounting, persistence, concurrency, recovery, and
   operational guarantees.

The current shipped API remains documented in [API_REFERENCE.md](../../docs/API_REFERENCE.md).
Historical gateway and authentication specs do not override this proposal's implementation baseline.

## Status and authority

The user requested detailed design only. Proposed decisions are reviewable recommendations, not
approval to implement, migrate, enable enforcement, deploy, or call a paid provider. The backend
design is normative for business semantics; the transport contract will be normative for wire
shapes. A conflict between them must be resolved before implementation, not silently guessed.

All monetary values in this proposal represent configured-tariff estimates and allowance control.
They are not customer invoices, provider invoices, cash balances, or payment obligations.
