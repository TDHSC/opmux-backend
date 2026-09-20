#[derive(Debug, Clone)]
pub struct RoutePlan {
    pub vendor_id: String,
    /// Catalog target identifier that owns configured pricing for this hop.
    pub target_id: String,
    /// Provider model requested on the wire for this hop.
    pub model_id: String,
    pub fallback_plans: Vec<RoutePlan>,
}
