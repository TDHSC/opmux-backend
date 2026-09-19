# Development Workflow Reference

This document is optional guidance, not a mandatory procedure or an approval gate. Consult the parts
that help with the current task; there is no requirement to read it for every change, produce spec
files, or request confirmation between stages. Explicit task instructions can request a different
process, including planning-only work or human review checkpoints.

Project constraints, validation requirements, and authorization boundaries remain in
[AGENTS.md](../../AGENTS.md) and the applicable engineering rules. Making this workflow optional
does not make those constraints optional.

## Suggested Working Loop

1. Understand the goal and acceptance conditions. Inspect the relevant code, tests, and existing
   requirements; distinguish intended behavior from what is currently implemented.
2. Plan only as much as the change needs. Reuse existing patterns and note assumptions that affect
   the result rather than designing an entire future system up front.
3. Implement a small, working increment. Add configuration, error handling, authentication, and
   other supporting behavior when the increment needs them; do not defer safeguards it already
   requires.
4. Run checks appropriate to the changed surface, fix relevant failures, and repeat as needed.
5. Summarize the changes, evidence, remaining limitations, and any blockers.

When the goal is clear, these steps can run continuously without human confirmation. Low-risk
implementation details can follow existing conventions or reasonable defaults. Handling unresolved
requirements or missing permissions is covered by the execution guidance in `AGENTS.md`.

## Proportional Planning

- **Small fixes, documentation, configuration, and behavior-preserving refactors:** a short plan or
  direct implementation is usually sufficient; no new spec documents are needed just for process.
- **Complex features or cross-module changes:** a concise design and task breakdown can help manage
  interfaces, trade-offs, testing, and delivery order.
- **Existing feature specs:** update relevant documents when behavior or decisions change rather
  than creating a parallel source of truth. Check their status against the implementation.

### Optional Spec Structure

When requested or useful for the agreed scope, the existing convention is:

- `specs/<feature>/requirements.md`: problem, user outcomes, and verifiable acceptance criteria.
- `specs/<feature>/design.md`: architecture, interfaces, data, trade-offs, testing, and security.
- `specs/<feature>/tasks.md`: small deliverable tasks, their requirement links, and progress.

These are optional artifacts, not three mandatory stages. EARS-style criteria such as
`When <trigger>, the <system> shall <response>` can make behavior precise. Diagrams can clarify
complex interactions. Neither a particular template nor sign-off is required unless the task
explicitly requests it; do not create documents solely to satisfy this reference.

## Incremental Implementation and Testing

A useful default for behavior changes is a failing test, the smallest implementation that passes it,
and then behavior-preserving cleanup. This is a recommended technique, not a requirement to invent a
runtime test for a documentation-only change.

- Service tests cover business rules and orchestration.
- Repository tests cover data and external-access boundaries; use controlled substitutes where
  possible and distinguish those checks from live-provider integration tests.
- Handler tests cover request/response mapping and error translation.
- Prefer deterministic tests with explicit inputs and outputs over deep fixtures or timing guesses.

For actual test commands, credential-gated provider tests, and scope-appropriate checks, see
[README.md](../../README.md#development-checks) and
[AGENTS.md](../../AGENTS.md#validation-and-external-calls). Use those instructions rather than
assuming all tests under `gateway/tests/` behave alike. Record which checks ran and which were
skipped; skipped or blocked checks are not passing evidence.
