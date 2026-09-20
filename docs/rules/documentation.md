# Rust Documentation Rules

## Core Principles

**Simple** - Easy to read and understand  
**Brief** - No unnecessary verbosity  
**Informative** - Contains all important messages  
**Consistent** - Uniform structure and style

## Documentation Structure

### 1. Module Documentation (`mod.rs`)

````rust
//! Module purpose and overview.
//!
//! Brief description of what this module does and its role in the system.
//!
//! # Architecture
//!
//! - **Handler** - Purpose and responsibility
//! - **Service** - Purpose and responsibility
//! - **Repository** - Purpose and responsibility
//!
//! # Usage
//!
//! ```bash
//! curl --noproxy '*' -X POST http://127.0.0.1:3000/api/v1/route \
//!   -H "X-API-Key: $INFERENCE_KEY" -H "Content-Type: application/json" \
//!   -d '{"prompt":"hello","metadata":{}}'
//! ```

/// Brief description of submodule purpose.
pub mod submodule;
````

### 2. Struct Documentation

```rust
/// Brief struct purpose.
///
/// Longer description if needed, explaining role in the system.
/// Future plans or important notes.
pub struct ServiceName {
    /// Field purpose and what it contains.
    field_name: Type,
}
```

### 3. Function Documentation

```rust
/// Brief function purpose (what it does).
///
/// # Flow (for complex functions)
/// 1. Step one description
/// 2. Step two description
///
/// # Parameters
/// - `param1` - Purpose and what it represents
/// - `param2` - Purpose and constraints
///
/// # Returns
/// What the function returns and its meaning
///
/// # Errors (if applicable)
/// When errors occur and what they mean
pub async fn function_name(param1: Type, param2: Type) -> Result<ReturnType, ErrorType> {
```

### 4. Error Documentation

```rust
/// Errors for [feature] operations.
///
/// Each variant represents a specific business operation failure,
/// providing clear context for debugging and monitoring.
#[derive(Debug, thiserror::Error)]
pub enum FeatureError {
    /// Brief error description (HTTP status if applicable).
    #[error("Error message")]
    ErrorVariant,
}
```

## Sections

Required: brief description; `# Parameters` for functions with inputs; `# Returns` for functions
with outputs.

Optional (use when helpful): `# Flow` for multi-step processes, `# Errors`, `# Examples`,
`# Future`.

## Writing Style

### Do ✅

- Start with action verbs ("Processes", "Retrieves", "Updates")
- Use present tense ("Returns user data")
- Be specific ("User identifier" not "ID")
- Include business context ("for context management")
- Mention HTTP status codes for errors
- Note future plans briefly

### Don't ❌

- Use filler ("This function", "This method")
- Repeat information from the function signature
- Write long paragraphs
- Include implementation details
- Use technical jargon without explanation

## Example

### Good ✅

```rust
/// Processes an AI routing request through the complete pipeline.
///
/// # Flow
/// 1. Validates the prompt and opaque metadata
/// 2. Selects the configured catalog route
/// 3. Executes the LLM call via `ExecutorService`
///
/// # Parameters
/// - `request` - AI routing request with prompt and opaque metadata
/// - `request_context` - Correlation IDs for downstream calls
///
/// # Returns
/// Complete AI response with metadata (cost, model, processing time)
pub async fn process_request(
    &self,
    request: IngressRequest,
    request_context: &RequestContext,
) -> Result<IngressResponse, IngressError>
```

### Poor ❌

```rust
/// This function processes a request
///
/// This method takes an IngressRequest and a String representing the user ID,
/// then it goes through several steps to process the request by calling various
/// services and then returns an IngressResponse or an error if something goes wrong
pub async fn process_request(/* ... */)
```

## Quick Checklist

- [ ] Brief, clear purpose statement
- [ ] Parameters documented with purpose
- [ ] Return value explained
- [ ] Complex logic broken into numbered steps
- [ ] Business context included
- [ ] HTTP status codes noted for errors
- [ ] No unnecessary verbosity
- [ ] Consistent with existing documentation
