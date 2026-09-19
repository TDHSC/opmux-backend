# Refactoring Rules

Applies when moving or restructuring existing code without changing behavior.

## Core Rules

- Keep existing interfaces: never change function signatures or trait definitions during a refactor.
- Only move code. Do not optimize, add features, or add abstraction layers or generics.
- Near-zero code increment: a refactor should not noticeably grow the codebase.

## Process

1. Identify the exact code to move (usually 2–10 lines).
2. Create the smallest module or file that can hold it.
3. Move the code as-is.
4. Update imports and call sites. Use `rg` or LSP "Find All References" for call sites; do not rely
   on semantic search for completeness.
5. Remove the original only after the build and tests pass.

## Code Increment Control

| Net increase | Action                               |
| ------------ | ------------------------------------ |
| < 20 lines   | Expected for a simple refactor       |
| > 50 lines   | Review for over-engineering          |
| > 100 lines  | Restart with a more minimal approach |

## Over-Engineering Red Flags

- New trait implementations that were not needed before
- Wrapper functions that did not exist before
- Extensive doc comments added during the move
- Async or borrow changes not present in the original
- New abstraction layers
