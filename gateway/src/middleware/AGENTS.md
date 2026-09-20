# MIDDLEWARE KNOWLEDGE BASE

## OVERVIEW

HTTP middleware stack for correlation IDs, metrics, protected-request deadlines, and authentication.

## WHERE TO LOOK

| Task               | Location                                   | Notes                                                |
| ------------------ | ------------------------------------------ | ---------------------------------------------------- |
| Middleware exports | `gateway/src/middleware/mod.rs`            | Re-exports and module list                           |
| Auth enforcement   | `gateway/src/middleware/auth.rs`           | API key checks                                       |
| Protected deadline | `gateway/src/middleware/deadline.rs`       | Auth, body, and execution                            |
| Correlation IDs    | `gateway/src/middleware/correlation_id.rs` | Root `http_request` span plus request ID injection   |
| Raw-body limits    | `gateway/src/app.rs` protected router      | `DefaultBodyLimit` plus `RequestBodyLimit` extension |

## CONVENTIONS

- Middleware order is defined in `gateway/src/app.rs` and should remain stable.
- Use `axum::middleware::from_fn` wrappers for middleware functions.
- Correlation opens the root request span before auth. Auth timing must end before `next.run`. Do
  not log credentials, digests, prompts, or raw bodies.

## ANTI-PATTERNS

- Do not add business logic here; middleware should be request/response concerns only.
