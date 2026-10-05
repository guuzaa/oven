//! Shared HTTP client shell for the completions and responses providers.
//!
//! Wire encode/decode stays in each protocol. This type owns the catalog,
//! headers, and the POST / `list_models` calls both transports repeat.

use std::collections::HashMap;

use isahc::http::header::HeaderMap;
use secrecy::SecretString;

use crate::domain::{ModelId, Request};
use crate::provider::http;
use crate::provider::model::ModelInfo;
use crate::provider::validate::validate_request;
use crate::provider::{ProviderError, ProviderName};

pub(crate) struct Endpoint {
    pub base_url: String,
    pub provider_name: ProviderName,
    pub api_key: SecretString,
    pub extra_headers: HeaderMap,
    model_catalog: HashMap<ModelId, ModelInfo>,
    client: isahc::HttpClient,
}

impl Endpoint {
    pub(crate) fn new(
        base_url: impl Into<String>,
        provider_name: ProviderName,
        api_key: impl Into<SecretString>,
        known_models: Vec<ModelInfo>,
        extra_headers: HeaderMap,
    ) -> Self {
        let model_catalog = known_models
            .into_iter()
            .map(|model| (ModelId::from(model.id.as_str()), model))
            .collect();
        Self {
            base_url: base_url.into(),
            provider_name,
            api_key: api_key.into(),
            extra_headers,
            model_catalog,
            client: isahc::HttpClient::new().expect("isahc HttpClient::new() should succeed"),
        }
    }

    pub(crate) fn known_models(&self) -> Vec<ModelInfo> {
        self.model_catalog.values().cloned().collect()
    }

    pub(crate) fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
        self.model_catalog
            .get(id)
            .or_else(|| self.model_catalog.get(&ModelId::from(id.wire_id())))
    }

    pub(crate) fn validate_known_model(
        &self,
        req: &Request,
        stream: bool,
    ) -> Result<(), ProviderError> {
        if let Some(model) = self.resolve_model(&req.model) {
            validate_request(req, model, stream)?;
        }
        Ok(())
    }

    pub(crate) fn merge_provider_options(
        mut body: serde_json::Value,
        options: &serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Value {
        if options.is_empty() {
            return body;
        }
        let object = body
            .as_object_mut()
            .expect("wire request always serializes to a JSON object");
        for (key, value) in options {
            object.insert(key.clone(), value.clone());
        }
        body
    }

    pub(crate) async fn post(
        &self,
        path: &str,
        body: &serde_json::Value,
        encode_error: impl FnOnce(String) -> ProviderError,
    ) -> Result<isahc::Response<isahc::AsyncBody>, ProviderError> {
        let body_bytes = serde_json::to_vec(body).map_err(|err| encode_error(err.to_string()))?;
        let headers = http::build_headers(&self.api_key, &self.extra_headers);
        let url = http::endpoint(&self.base_url, path);
        http::post_json(&self.client, &headers, url, body_bytes).await
    }

    pub(crate) async fn list_models(
        &self,
        map_json_err: impl FnOnce(serde_json::Error) -> ProviderError,
    ) -> Result<Vec<ModelInfo>, ProviderError> {
        let headers = http::build_headers(&self.api_key, &self.extra_headers);
        http::list_models(
            &self.client,
            &headers,
            &self.base_url,
            &self.provider_name,
            map_json_err,
        )
        .await
    }
}
