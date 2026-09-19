// Repository Layer - LLM vendor management and direct API calls

use super::{
    config::ExecutorConfig,
    error::ExecutorError,
    models::{ExecutionParams, ExecutionResult},
    vendors::{openai::OpenAIVendor, traits::LLMVendor},
};
use std::collections::HashMap;
use std::sync::Arc;

/// Repository for LLM vendor management and direct API calls.
///
/// Manages vendor registration and provides direct access to LLM APIs.
/// Does NOT contain business logic (retry, fallback) - that belongs in Service layer.
pub struct ExecutorRepository {
    /// Vendor registry: vendor_id → vendor instance
    pub(crate) vendors: HashMap<String, Arc<dyn LLMVendor>>,
}

impl ExecutorRepository {
    /// Creates ExecutorRepository from configuration.
    ///
    /// Automatically initializes all configured vendors.
    ///
    /// # Parameters
    /// - `config` - Executor configuration with vendor settings
    ///
    /// # Returns
    /// ExecutorRepository instance with initialized vendors
    ///
    /// # Errors
    /// Returns `NoVendorsConfigured` if no vendors are configured
    pub fn from_config(config: ExecutorConfig) -> Result<Self, ExecutorError> {
        let mut vendors: HashMap<String, Arc<dyn LLMVendor>> = HashMap::new();

        // Initialize OpenAI vendor if configured
        if let Some(openai_config) = &config.openai {
            let vendor = OpenAIVendor::new(openai_config.clone())?;
            vendors.insert("openai".to_string(), Arc::new(vendor));
            tracing::info!("Initialized OpenAI vendor");
        }

        // Future: Initialize Anthropic vendor if configured
        // if let Some(anthropic_key) = &config.anthropic_api_key {
        //     let vendor = AnthropicVendor::new(anthropic_key.clone());
        //     vendors.insert("anthropic".to_string(), Arc::new(vendor));
        //     tracing::info!("Initialized Anthropic vendor");
        // }

        // Validate that at least one vendor is configured
        if vendors.is_empty() {
            return Err(ExecutorError::NoVendorsConfigured);
        }

        Ok(Self { vendors })
    }

    /// Returns the number of registered vendors.
    ///
    /// Useful for logging and monitoring vendor availability.
    pub fn vendor_count(&self) -> usize {
        self.vendors.len()
    }

    /// Lists all registered vendor names.
    ///
    /// # Returns
    /// Vector of vendor identifiers (e.g., ["openai", "anthropic"])
    pub fn list_vendor_names(&self) -> Vec<String> {
        self.vendors.keys().cloned().collect()
    }

    /// Selects vendor by vendor_id.
    ///
    /// # Parameters
    /// - `vendor_id` - Vendor identifier (e.g., "openai", "anthropic")
    ///
    /// # Returns
    /// Arc reference to the vendor implementation
    ///
    /// # Errors
    /// Returns `UnsupportedVendor` if vendor_id is not found in registry
    pub fn get_vendor(
        &self,
        vendor_id: &str,
    ) -> Result<Arc<dyn LLMVendor>, ExecutorError> {
        self.vendors
            .get(vendor_id)
            .cloned()
            .ok_or_else(|| ExecutorError::UnsupportedVendor(vendor_id.to_string()))
    }

    /// Calls LLM API directly (no retry, no fallback).
    ///
    /// This is a CHILD SPAN. It automatically inherits `request_id` from parent.
    ///
    /// This is a pure data access method - business logic (retry, fallback)
    /// should be handled by the Service layer.
    ///
    /// # Parameters
    /// - `vendor_id` - Vendor identifier
    /// - `model_id` - Model identifier
    /// - `params` - Execution parameters
    ///
    /// # Returns
    /// Execution result with AI response and metrics
    ///
    /// # Errors
    /// Returns error if:
    /// - Vendor not found
    /// - Model not supported
    /// - API call fails
    #[tracing::instrument(
        skip(self, params),
        fields(
            vendor_id = %vendor_id,
            model_id = %model_id,
            max_tokens = params.max_tokens,
        )
    )]
    pub async fn call_llm(
        &self,
        vendor_id: &str,
        model_id: &str,
        params: &ExecutionParams,
    ) -> Result<ExecutionResult, ExecutorError> {
        tracing::debug!("Calling LLM API");

        // Get vendor
        let vendor = self.get_vendor(vendor_id)?;

        // Validate model support
        if !vendor.supports_model(model_id) {
            tracing::warn!("Model not supported by vendor");
            return Err(ExecutorError::UnsupportedModel(
                vendor_id.to_string(),
                model_id.to_string(),
            ));
        }

        // Direct API call (no retry, no fallback)
        let result = vendor.execute(model_id, params.clone()).await?;

        tracing::debug!(
            prompt_tokens = result.prompt_tokens,
            completion_tokens = result.completion_tokens,
            total_cost = result.total_cost,
            "LLM API call completed"
        );

        Ok(result)
    }
}
