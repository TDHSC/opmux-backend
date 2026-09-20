#[derive(Debug, Clone)]
pub struct RoutePlan {
    pub vendor_id: String,
    /// Catalog target identifier that owns configured pricing for this hop.
    pub target_id: String,
    /// Provider model requested on the wire for this hop.
    pub model_id: String,
    /// Inclusive output-token cap of this hop from the operator catalog.
    ///
    /// Fallback hops that cannot satisfy the already-validated `max_tokens`
    /// are skipped without changing generation parameters.
    pub max_output_tokens: u32,
    pub fallback_plans: Vec<RoutePlan>,
}
