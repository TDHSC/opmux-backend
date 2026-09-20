//! Configured default and named route selection.

use super::error::IngressError;
use crate::core::{
    config::{Catalog, Target},
    contracts::RoutePlan,
};

/// Catalog route and flat target plan selected for one request.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedRoute {
    /// Configured route identifier.
    pub route_id: String,
    /// Primary target identifier from the catalog.
    pub target_id: String,
    /// Flat execution plan for the executor boundary.
    pub plan: RoutePlan,
}

/// Selects a configured route and builds its flat target plan.
///
/// Omitted `requested_route` uses the catalog default. Omitted or `true`
/// `allow_fallback` includes the configured fallback chain. `false` limits the
/// plan to the primary target and does not disable that target's retries.
///
/// # Parameters
/// - `catalog` - Validated operator catalog
/// - `requested_route` - Optional client-selected route name
/// - `allow_fallback` - Optional client fallback opt-out
///
/// # Returns
/// Resolved route identifier and flat `RoutePlan`
///
/// # Errors
/// Returns `InvalidRequest` for an unknown route before any provider access.
pub(crate) fn resolve_route(
    catalog: &Catalog,
    requested_route: Option<&str>,
    allow_fallback: Option<bool>,
) -> Result<ResolvedRoute, IngressError> {
    let route_id = match requested_route {
        None => catalog.default_route.clone(),
        Some(name) => {
            if !is_configured_route(catalog, name) {
                return Err(IngressError::InvalidRequest("Unknown route".to_string()));
            }
            name.to_string()
        }
    };

    let route = catalog
        .routes
        .get(&route_id)
        .ok_or(IngressError::RequestOrchestrationFailed)?;
    let primary = target(catalog, &route.primary)?;
    let include_fallbacks = allow_fallback != Some(false);
    let fallback_plans = if include_fallbacks {
        route
            .fallbacks
            .iter()
            .map(|id| target(catalog, id).map(plan_for_target))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    Ok(ResolvedRoute {
        route_id,
        target_id: route.primary.clone(),
        plan: RoutePlan {
            vendor_id: primary.vendor.as_str().to_string(),
            model_id: primary.model.clone(),
            fallback_plans,
        },
    })
}

fn is_configured_route(catalog: &Catalog, name: &str) -> bool {
    !name.is_empty() && name.trim() == name && catalog.routes.contains_key(name)
}

fn target<'a>(catalog: &'a Catalog, id: &str) -> Result<&'a Target, IngressError> {
    catalog
        .targets
        .get(id)
        .ok_or(IngressError::RequestOrchestrationFailed)
}

fn plan_for_target(target: &Target) -> RoutePlan {
    RoutePlan {
        vendor_id: target.vendor.as_str().to_string(),
        model_id: target.model.clone(),
        fallback_plans: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Settings;

    fn catalog() -> Catalog {
        let mut catalog = Settings::for_tests().catalog;
        catalog
            .routes
            .get_mut("default")
            .expect("default route")
            .fallbacks = vec!["secondary".to_string()];
        catalog
    }

    #[test]
    fn omitted_route_selects_default_primary_and_configured_fallbacks() {
        let resolved = resolve_route(&catalog(), None, None).expect("default route");
        assert_eq!(resolved.route_id, "default");
        assert_eq!(resolved.target_id, "primary");
        assert_eq!(resolved.plan.vendor_id, "openai");
        assert_eq!(resolved.plan.model_id, "example-chat-model");
        assert_eq!(resolved.plan.fallback_plans.len(), 1);
        assert_eq!(
            resolved.plan.fallback_plans[0].model_id,
            "example-chat-model-mini"
        );
        assert!(resolved.plan.fallback_plans[0].fallback_plans.is_empty());
    }

    #[test]
    fn named_route_selects_its_configured_primary() {
        let resolved =
            resolve_route(&catalog(), Some("fast"), None).expect("named route");
        assert_eq!(resolved.route_id, "fast");
        assert_eq!(resolved.target_id, "secondary");
        assert_eq!(resolved.plan.model_id, "example-chat-model-mini");
        assert!(resolved.plan.fallback_plans.is_empty());
    }

    #[test]
    fn allow_fallback_false_omits_configured_fallbacks() {
        let resolved = resolve_route(&catalog(), None, Some(false)).expect("opt-out");
        assert_eq!(resolved.plan.model_id, "example-chat-model");
        assert!(resolved.plan.fallback_plans.is_empty());
    }

    #[test]
    fn allow_fallback_true_keeps_configured_fallbacks() {
        let resolved = resolve_route(&catalog(), None, Some(true)).expect("opt-in");
        assert_eq!(resolved.plan.fallback_plans.len(), 1);
    }

    #[test]
    fn unknown_or_blank_route_is_invalid() {
        for name in ["not-a-configured-route", "", " fast ", "default "] {
            match resolve_route(&catalog(), Some(name), None) {
                Err(IngressError::InvalidRequest(message)) => {
                    assert_eq!(message, "Unknown route");
                }
                other => panic!("expected unknown route, got {other:?}"),
            }
        }
    }
}
