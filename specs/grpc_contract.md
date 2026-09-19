# gRPC Contract Design

Protocol Buffers clearly define:

1. **Service**: A collection of functionalities that a microservice provides. For example,
   MemoryService.
2. **RPC Methods**: Specific function names for each functionality. For example, GetContext.
3. **Messages**: What data needs to be passed when calling these functions (request messages) and
   what data will be returned (response messages).

## Service Responsibilities

- **Memory Service** (deferred to future, renamed to Context Engine, not needed for current MVP):
  Its sole responsibility is "providing context". It should not care about what the context will be
  used for (routing, rewriting, or others). So its interface is simply GetContext.

- **Router Service**: Its sole responsibility is "optimization and decision-making". It receives
  context and requests, then outputs an optimized routing strategy (RoutePlan). It should not
  execute LLM requests itself; actual execution is handled by Gateway's Executor layer.

- **RewriteService** (upcoming): Its sole responsibility is "request rewriting". Input is the
  original request and some rules, output is the modified request.

- **ValidationService** (upcoming): Its sole responsibility is "validation". Input is the request,
  output is "whether valid" and "error list".

## Design Considerations for RPC Contracts

When designing RPC contracts, consider:

- **Request Messages**: What is the minimum information an RPC method needs to complete its work?
  Make this information explicit as request fields.
- **Response Messages**: What return results do callers care about most? What data on success? What
  information on failure?

### Question 1: "What does this service consume and produce?"

This is the most important question - it defines the core value of the service. Forget all technical
details, just think about its inputs and outputs.

- **Thinking approach**: Imagine it as a pure function `fn(input) -> output`.
- **Applied to Opmux (stateless MVP)**:
  - **RouterService**:
    - Consumes: User's original request (original_payload)
    - Produces: An optimized routing strategy (RoutePlan) containing vendor_id, model_id, execution
      parameters, etc. If cache hit, directly returns cached_response.
  - **RewriteService**:
    - Consumes: User's original request (original_payload)
    - Produces: A modified request (modified_payload)
  - **ValidationService**:
    - Consumes: User's original request (original_payload)
    - Produces: A validation report (ValidationReport) containing whether it passed and error list

### Question 2: "What 'extra information' does the caller need to thrive?"

Services are not isolated - they are used by others (callers, such as Gateway). A "useful" service
should provide sufficient "context" and "observability" information beyond completing its core task.

- **Thinking approach**: If I'm the caller, what information do I need for troubleshooting when
  calls fail? What data do I need for performance monitoring?

- **Applied to Opmux**:
  - **Troubleshooting**: "How do I correlate Gateway logs with RouterService logs?"
    - → Need to return a `trace_id` in the response.

  - **Handling failures**: "The call failed, should I retry?"
    - → Need to include a `retryable` boolean in error information.

  - **Performance monitoring**: "How long did this call take?"
    - → Need to return `duration_ms` in the response.

  - **Unified standards**: "Does every service return this information? Is the format the same?"
    - → This is why we design `common.proto` and have all services use `RequestMeta` and
      `ResponseMeta`.

**Output of this question**: You'll get those non-business core fields that make your microservice
system robust, transparent, and easy to maintain.

### Question 3: "What do I least want to do a year from now?"

This is a "defensive" question about the future and technical debt. It forces you to think about API
evolution.

- **Thinking approach**: Imagine a year from now, the product manager comes with new requirements.
  What's the worst thing that could happen requiring large-scale refactoring?

- **Applied to Opmux**:
  - **Your worst fear**: "The PM says: 'We're launching user history!' This means RouterService
    needs context now. Then you discover you must modify all service .proto files, create a v2
    version, and spend months migrating all callers from v1 to v2."

  - **How to avoid this nightmare**: To avoid it, we make choices today. We can reserve a `context`
    field in the v1 interface and explicitly document "this field is ignored in MVP phase".

  - **Result**: A year later when new requirements arrive, you only need to upgrade Gateway and
    RouterService's internal logic to use this pre-existing field. No interface changes, no painful
    migration. You save months of work and can focus on developing new features.

## Proto Definitions

### proto/common/v1/common.proto

```protobuf
syntax = "proto3";

package opmux.common.v1;

// Request metadata attached to all gRPC requests
message RequestMeta {
  string request_id = 1;    // Unique request ID
  string client_id = 2;     // Client ID initiating the request, provided by Gateway's auth layer

  // W3C Trace Context standard traceparent string.
  // Format: "00-0af76519-16cd43dd-b7ad6b71-01"
  // Passes trace_id and parent_span_id to services.
  string traceparent = 3;   // For distributed tracing

  // Final deadline for the task, in Unix epoch milliseconds
  int64 deadline_ms = 4;
}

// Response metadata attached to all gRPC responses
message ResponseMeta {
  int32 duration_ms = 1;           // Service processing duration
  repeated string warnings = 2;    // Non-fatal warnings during execution
}

// Unified business error structure
message ErrorDetail {
  string error_code = 1;  // Internal business error code
  string message = 2;     // Error message
  bool retryable = 3;     // Whether caller can retry (fixed typo: was retryble)
}

// LLM invocation cost information
message Cost {
  int64 prompt_tokens = 1;
  int64 completion_tokens = 2;
  double total_cost_usd = 3;
}
```

### proto/router/v1/router.proto

```protobuf
syntax = "proto3";

package opmux.router.v1;

import "opmux/common/v1/common.proto";
import "google/protobuf/struct.proto";

service RouterService {
  // Optimize and determine the best routing strategy (does NOT execute LLM calls)
  rpc OptimizeRoute(OptimizeRouteRequest) returns (OptimizeRouteResponse);
}

message OptimizeRouteRequest {
  opmux.common.v1.RequestMeta meta = 1;
  google.protobuf.Struct original_payload = 2;

  // NOTE: This field is reserved for future MemoryService integration.
  // In V1 implementation, the server will ignore this field's content.
  map<string, string> context = 3;
}

// Routing execution plan (for Gateway's Executor layer)
message RoutePlan {
  string vendor_id = 1;     // Vendor identifier: "openai", "anthropic", "cohere"
  string model_id = 2;      // Model identifier: "gpt-4", "claude-3-opus"

  // Fallback strategy chain (try sequentially if primary fails)
  repeated RoutePlan fallback_plans = 3;
}

message OptimizeRouteResponse {
  opmux.common.v1.ResponseMeta meta = 1;
  repeated opmux.common.v1.ErrorDetail errors = 2;

  // --- Core business output ---
  RoutePlan optimized_plan = 3;  // Optimized best routing strategy

  // Observability information
  string optimization_reason = 4;  // Why this strategy was chosen (for debugging/monitoring)
}
```

**Architecture Explanation:**

RouterService is only responsible for "strategy decisions", not executing actual LLM calls. Complete
flow:

```
Client → Gateway → RouterService: Request strategy
                 ← Returns RoutePlan

Gateway → Executor Layer: Call actual LLM based on RoutePlan
        ← Returns LLM response
Gateway → Client: Return response
```

**Important Design Note:**

RouterService receives the complete `original_payload` from the client, which contains all execution
parameters (temperature, max_tokens, top_p, etc.) and user preferences. RouterService can parse
these parameters from `original_payload` when needed for routing decisions, but does NOT return
modified parameters in the response.

Why we don't pass execution parameters separately:

- **Single source of truth**: All parameters are in `original_payload`, avoiding data duplication
- **Simplicity**: RouterService doesn't modify parameters in MVP - it only selects vendor/model
- **Flexibility**: Gateway/Executor extracts parameters directly from `original_payload` for LLM
  calls
- **Future-proof**: If RouterService needs to modify parameters later, we can add `execution_params`
  back to RoutePlan

Example flow:

```
Client request:
{
  "messages": [...],
  "temperature": 0.7,
  "max_tokens": 1000
}

RouterService:
1. Parses original_payload to understand request characteristics
2. May consider temperature/max_tokens for routing decisions
3. Returns: { vendor_id: "openai", model_id: "gpt-4" }

Gateway/Executor:
1. Extracts temperature=0.7, max_tokens=1000 from original_payload
2. Calls OpenAI API with these parameters
```

Executor Layer is an internal module within Gateway (not a gRPC service), responsible for:

- Integrating vendor SDKs (OpenAI, Anthropic, etc.)
- Handling HTTP calls, retries, timeouts
- Supporting streaming responses (SSE/WebSocket)
- Extracting execution parameters from original_payload

### proto/rewrite_service/v1/rewrite.proto

```protobuf
syntax = "proto3";

package opmux.rewrite.v1;

import "opmux/common/v1/common.proto";
import "google/protobuf/struct.proto";

service RewriteService {
  // Rewrite request
  rpc RewriteRequest(RewriteRequestRequest) returns (RewriteRequestResponse);
}

message RewriteRequestRequest {
  opmux.common.v1.RequestMeta meta = 1;
  google.protobuf.Struct original_payload = 2;

  // NOTE: This field is reserved for future MemoryService integration.
  // In V1 implementation, the server will ignore this field's content.
  map<string, string> context = 3;

  // For executing platform-level, predefined rules (e.g., PII masking)
  repeated string rewrite_rules = 4;

  // For executing user-defined, dynamic template filling.
  // If this field exists, service will execute template filling logic.
  google.protobuf.Struct custom_template = 5;
}

message RewriteRequestResponse {
  opmux.common.v1.ResponseMeta meta = 1;
  repeated opmux.common.v1.ErrorDetail errors = 2;

  // -- Core business output --
  google.protobuf.Struct modified_payload = 3;  // Rewritten request body
}
```

### proto/validation_service/v1/validation.proto

```protobuf
syntax = "proto3";

package opmux.validation.v1;

import "opmux/common/v1/common.proto";
import "google/protobuf/struct.proto";

service ValidationService {
  // Validate request
  rpc ValidateRequest(ValidateRequestRequest) returns (ValidateRequestResponse);
}

message ValidateRequestRequest {
  opmux.common.v1.RequestMeta meta = 1;
  google.protobuf.Struct original_payload = 2;

  // NOTE: This field is reserved for future MemoryService integration.
  // In V1 implementation, the server will ignore this field's content.
  map<string, string> context = 3;
}

message ValidationFailure {
  string field = 1;   // Field that failed validation
  string reason = 2;  // Reason for failure
}

message ValidateRequestResponse {
  opmux.common.v1.ResponseMeta meta = 1;
  repeated opmux.common.v1.ErrorDetail errors = 2;  // Errors during service execution

  // --- Core business output ---
  bool is_valid = 3;                          // Whether validation passed
  repeated ValidationFailure failures = 4;    // Specific validation failure points
}
```

## Field Promotion Guidelines

"Which fields should be promoted to first-class citizens, and which should remain in generic
containers?" - There's no single answer, but we can follow a very clear thinking framework.

**The core question of this framework is: Does my service need to "understand" this field?**

Let's break down this question with a "promotion criteria" checklist:

### Criterion 1: Is this field critical to "core business logic"?

If a field's value fundamentally changes how your current service behaves, it should be "promoted"
to a first-class citizen field.

**Positive examples (should promote):**

- **deadline_ms**: We extract it from `original_payload` and put it in `RequestMeta` because
  RouterService's core logic depends on it. The service needs to understand this field to decide
  "Should I use fast/expensive real-time routing, or slow/cheap batch routing?" This field directly
  affects routing decisions.

- **client_id**: Similarly, if RouterService needs to provide different routing strategies based on
  different customer tiers (client_id linked to customer information), then client_id must be a
  first-class citizen field it can directly understand.

**Negative examples (should NOT promote):**

- **temperature**: Does RouterService's routing logic care about temperature? Usually not. It
  doesn't need to change routing decisions because temperature is 0.5 vs 0.8. This field just needs
  to be passed to the LLM eventually. So it should stay in `original_payload`.

### Criterion 2: Does this field belong to "cross-cutting system-level concerns"?

Some information, such as observability, security, and general metadata, needs to be understood and
handled by every microservice in the system in a standardized way. This information must be
"first-class citizens".

**Positive examples:**

- **traceparent**: This is the most typical example. Every service in the system needs to
  participate in distributed tracing. If we hide it in `original_payload`, every service would need
  to parse that complex JSON to find it, which would be very messy and fragile. Putting it in
  `RequestMeta` allows middleware to handle it uniformly and automatically.

- **request_id**: Used for log tracing and idempotency checks, also a system-level field every
  service needs to care about.

### Criterion 3: Is this field's structure "stable" and "explicit"?

First-class citizen fields should have stable structure and explicit types. Data that is
structurally complex, variable, or "opaque" to the current service is better suited for generic
containers.

**Positive examples:**

- **deadline_ms** will always be an int64. **needs_rewrite** will always be a bool. Their meanings
  are very stable.

**Negative examples:**

- **original_payload**: This is precisely the core value of `original_payload`. User request body
  (payload) structure is extremely variable. OpenAI's input is a messages array, Anthropic is a text
  block, Cohere is yet another format. We cannot and absolutely should not model every LLM's input
  format in the gRPC contract.

- Therefore, we use `google.protobuf.Struct` as a "generic JSON container" to carry
  `original_payload`. For RouterService, it's an "opaque black box". Its final destination is the
  downstream LLM, and RouterService just needs to pass it through unchanged.

---

### Summary and Final "Litmus Test"

You can use a simple "litmus test" for decisions:

> **If your service code frequently writes `if payload.get("some_field") == ...` to make decisions,
> then `some_field` should be "promoted" to an independent, strongly-typed first-class citizen
> field.**

> **If your service just passes data from input to output unchanged, or never cares about its
> content, then it should continue to stay in a generic container like `original_payload`.**
