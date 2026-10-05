//! `Router`：哪家 provider 拥有哪个模型。调用走 [`Client`]，重试走
//! [`crate::RetryingProvider`]。
//!
//! 派发顺序：
//! 1. slug 的 vendor 段匹配已注册 provider（再按 `:variant` / 目录默认协议选实现）；
//! 2. 各 provider 静态目录（[`Provider::resolve_model`]，先注册者胜出）；
//! 3. 均未命中则返回 [`RouterError::UnknownModel`]。
//!
//! 裸 id 在只注册了一家 vendor 时会先补成 `vendor/wire-id`。

use std::collections::HashSet;
use std::ops::Deref;
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use futures::stream::BoxStream;
use thiserror::Error;

use crate::domain::{ModelId, Request, Response, StreamEvent};
use crate::provider::catalog;
use crate::provider::model::ModelInfo;
use crate::provider::{Provider, ProviderError, ProviderKind, ProviderName};

/// `Router` 派发失败。调用失败是 [`ProviderError`]，由 [`Client`] 返回。
#[derive(Debug, Error)]
pub enum RouterError {
    /// 未注册任何 provider。
    #[error("no provider registered")]
    NoProviderRegistered,
    /// 没有任何已注册 provider 或规则匹配该模型。
    #[error("no registered provider or route matches model {0}")]
    UnknownModel(ModelId),
}

impl From<RouterError> for ProviderError {
    fn from(err: RouterError) -> Self {
        match err {
            RouterError::NoProviderRegistered => ProviderError::NoProviderRegistered,
            RouterError::UnknownModel(model) => ProviderError::UnknownModel(model),
        }
    }
}

/// 厂商表：哪个 provider 拥有哪个模型。不发起调用，也不重试。
#[derive(Default)]
pub struct Router {
    /// 按注册顺序保存的 provider。
    providers: Vec<Box<dyn Provider>>,
}

impl Router {
    /// 创建空路由。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个 provider，返回 `self` 以支持链式调用。
    ///
    /// 接受具体类型，也接受 [`ProviderBuilder`](crate::ProviderBuilder) 返回的
    /// `Box<dyn Provider>`。注册顺序决定目录扫描的归属：同一模型被多个
    /// provider 的目录命中时，先注册者胜出。
    ///
    /// 同一厂商的多套协议（Completions + Responses）应多次 [`register`]。
    /// 更新某一条时用 [`upsert`](Self::upsert)，键是 vendor slug 加协议。
    pub fn register(&mut self, provider: impl Provider + 'static) -> &mut Self {
        self.providers.push(Box::new(provider));
        self
    }

    /// 按 vendor slug 和协议登记：已有同键则替换，否则追加。
    ///
    /// 同一厂商的 Completions 和 Responses 是两条，互不影响。
    pub fn upsert(&mut self, provider: impl Provider + 'static) -> &mut Self {
        let provider: Box<dyn Provider> = Box::new(provider);
        let slug = provider.provider_name().slug().to_string();
        let protocol = provider.protocol();
        if let Some(index) = self.providers.iter().position(|existing| {
            existing.provider_name().slug() == slug && existing.protocol() == protocol
        }) {
            self.providers[index] = provider;
        } else {
            self.providers.push(provider);
        }
        self
    }

    /// 各 provider 静态目录的并集。同一 slug 只保留先注册的那条。
    pub fn known_models(&self) -> Vec<ModelInfo> {
        let mut models = Vec::new();
        let mut seen = HashSet::new();
        for provider in &self.providers {
            for mut model in provider.known_models() {
                let slug = model.slug();
                if seen.insert(slug.clone()) {
                    model.id = slug;
                    models.push(model);
                }
            }
        }
        models
    }

    /// 静态目录里该模型的元数据。未命中表示没有声明，不代表不能派发。
    pub fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
        let id = self.qualify(id);
        self.providers
            .iter()
            .find_map(|provider| provider.resolve_model(&id))
    }

    /// 裸 id 在只注册了一家 vendor 时补上前缀；已有 vendor 则规范别名。
    pub fn qualify(&self, model: &ModelId) -> ModelId {
        if model.vendor().is_some() {
            return model.qualify(model.vendor().expect("vendor present"));
        }
        if let Some(vendor) = self.single_vendor_slug() {
            return model.qualify(&vendor);
        }
        model.clone()
    }

    /// 解析 `model` 对应的 provider。详见模块文档中的派发优先级。
    pub fn provider(&self, model: &ModelId) -> Result<&dyn Provider, RouterError> {
        if self.providers.is_empty() {
            return Err(RouterError::NoProviderRegistered);
        }

        let model = self.qualify(model);

        // slug vendor → 已注册 provider，再按协议挑选实现。
        if let Some(vendor) = model.vendor()
            && let Some(provider) = self.provider_for_vendor(vendor, &model)
        {
            return Ok(provider);
        }

        // 静态目录扫描：按注册顺序取首个命中。
        if let Some(provider) = self
            .providers
            .iter()
            .find(|provider| provider.resolve_model(&model).is_some())
        {
            return Ok(provider.as_ref());
        }

        Err(RouterError::UnknownModel(model))
    }

    fn single_vendor_slug(&self) -> Option<String> {
        let mut slugs = self
            .providers
            .iter()
            .map(|provider| provider.provider_name().slug().to_string())
            .collect::<Vec<_>>();
        slugs.sort();
        slugs.dedup();
        if slugs.len() == 1 { slugs.pop() } else { None }
    }

    fn provider_for_vendor(&self, vendor: &str, model: &ModelId) -> Option<&dyn Provider> {
        let protocol = self.resolve_protocol(model);
        let matches: Vec<&dyn Provider> = self
            .providers
            .iter()
            .filter(|provider| provider.provider_name().matches_vendor(vendor))
            .map(|provider| provider.as_ref())
            .collect();
        pick_protocol(matches, protocol)
    }

    fn resolve_protocol(&self, model: &ModelId) -> ProviderKind {
        if let Some(variant) = model.variant() {
            return match variant.to_ascii_lowercase().as_str() {
                "responses" => ProviderKind::Responses,
                "messages" => ProviderKind::Messages,
                _ => ProviderKind::Completions,
            };
        }
        if let Some(info) = self.lookup_catalog(model) {
            return info.default_protocol();
        }
        ProviderKind::Completions
    }

    fn lookup_catalog(&self, model: &ModelId) -> Option<ModelInfo> {
        if let Some(info) = self.resolve_model(model) {
            return Some(info.clone());
        }
        let models = catalog::all_models();
        models
            .iter()
            .find(|info| {
                info.id == model.wire_id()
                    && info.provider.matches_vendor(model.vendor().unwrap_or(""))
            })
            .or_else(|| models.iter().find(|info| info.id == model.wire_id()))
            .cloned()
    }
}

fn pick_protocol(matches: Vec<&dyn Provider>, protocol: ProviderKind) -> Option<&dyn Provider> {
    if matches.is_empty() {
        return None;
    }
    if let Some(exact) = matches
        .iter()
        .copied()
        .find(|provider| provider.protocol() == Some(protocol))
    {
        return Some(exact);
    }
    if matches.len() == 1 {
        return Some(matches[0]);
    }
    matches
        .iter()
        .copied()
        .find(|provider| provider.protocol().is_none())
        .or_else(|| matches.first().copied())
}

/// 厂商表上的调用入口。[`Provider`] 的 `complete` / `stream` 是唯一的调用方法。
///
/// 表操作（[`Router::provider`]、[`Router::qualify`]、[`Router::resolve_model`]、
/// [`Router::known_models`]）透过 [`Deref`] 使用。
pub struct Client {
    router: Router,
}

impl From<Router> for Client {
    fn from(router: Router) -> Self {
        Self { router }
    }
}

impl Deref for Client {
    type Target = Router;

    fn deref(&self) -> &Self::Target {
        &self.router
    }
}

#[async_trait]
impl Provider for Client {
    async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
        let provider = self.router.provider(&req.model)?;
        provider.complete(req).await
    }

    async fn stream(
        &self,
        req: &Request,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        let provider = self.router.provider(&req.model)?;
        provider.stream(req).await
    }

    fn known_models(&self) -> Vec<ModelInfo> {
        self.router.known_models()
    }

    fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
        self.router.resolve_model(id)
    }

    fn provider_name(&self) -> ProviderName {
        match self.router.single_vendor_slug() {
            Some(slug) => ProviderName::from(slug.as_str()),
            None => ProviderName::Custom("router".into()),
        }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let mut models = Vec::new();
        let mut seen = HashSet::new();
        for provider in &self.router.providers {
            for mut model in provider.list_models().await? {
                let slug = model.slug();
                if seen.insert(slug.clone()) {
                    model.id = slug;
                    models.push(model);
                }
            }
        }
        Ok(models)
    }
}

/// 一份可替换的 [`Client`] 快照。
///
/// [`load`](Self::load) 克隆出 `Arc<Client>`，拿着它跨 `.await` 不会挡住下一次
/// [`replace`](Self::replace)。正在进行的请求继续用旧 provider；下一次读取看到新表。
#[derive(Clone)]
pub struct RouterHandle {
    inner: Arc<RwLock<Arc<Client>>>,
}

impl RouterHandle {
    pub fn new(router: Router) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Arc::new(Client::from(router)))),
        }
    }

    pub fn load(&self) -> Arc<Client> {
        Arc::clone(&self.inner.read().unwrap_or_else(PoisonError::into_inner))
    }

    pub fn replace(&self, router: Router) {
        *self.inner.write().unwrap_or_else(PoisonError::into_inner) =
            Arc::new(Client::from(router));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use futures::StreamExt;

    use crate::domain::message::Role;
    use crate::provider::model::{ModelCapabilities, ModelInfo};

    fn model_info(id: &str, provider: ProviderName) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            provider,
            context_window: 1000,
            max_output_tokens: 100,
            capabilities: ModelCapabilities::default(),
            pricing: None,
            protocols: Vec::new(),
        }
    }

    /// 一个带模型目录、可记录调用并可注入失败的 stub provider。
    struct StubProvider {
        name: ProviderName,
        catalog: HashMap<ModelId, ModelInfo>,
        calls: Arc<Mutex<Vec<ProviderName>>>,
        fail: bool,
        protocol: Option<ProviderKind>,
    }

    impl StubProvider {
        fn new(name: ProviderName, models: &[&str]) -> Self {
            let catalog = models
                .iter()
                .map(|id| (ModelId::from(*id), model_info(id, name.clone())))
                .collect();
            Self {
                name,
                catalog,
                calls: Arc::new(Mutex::new(Vec::new())),
                fail: false,
                protocol: None,
            }
        }

        fn with_protocol(mut self, protocol: ProviderKind) -> Self {
            self.protocol = Some(protocol);
            self
        }

        fn failing(name: ProviderName, models: &[&str]) -> Self {
            let mut provider = Self::new(name, models);
            provider.fail = true;
            provider
        }

        fn calls(&self) -> Arc<Mutex<Vec<ProviderName>>> {
            self.calls.clone()
        }
    }

    #[async_trait]
    impl Provider for StubProvider {
        async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
            if self.fail {
                return Err(ProviderError::Auth("stub auth failure".to_string()));
            }
            self.calls.lock().unwrap().push(self.name.clone());
            Ok(Response {
                id: "stub-response".to_string(),
                model: req.model.as_str().to_owned(),
                role: Role::Assistant,
                content: Vec::new(),
                stop_reason: None,
                usage: None,
            })
        }

        async fn stream(
            &self,
            _req: &Request,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
            if self.fail {
                return Err(ProviderError::Auth("stub auth failure".to_string()));
            }
            self.calls.lock().unwrap().push(self.name.clone());
            let events = futures::stream::iter(vec![Ok(StreamEvent::MessageStop)]);
            Ok(Box::pin(events))
        }

        fn known_models(&self) -> Vec<ModelInfo> {
            self.catalog.values().cloned().collect()
        }

        fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
            self.catalog.get(id)
        }

        fn provider_name(&self) -> ProviderName {
            self.name.clone()
        }

        fn protocol(&self) -> Option<ProviderKind> {
            self.protocol
        }
    }

    fn request(model: &str) -> Request {
        Request::builder().model(model).build().unwrap()
    }

    #[test]
    fn no_provider_registered_is_error() {
        let router = Router::new();
        assert!(matches!(
            router.provider(&ModelId::from("anything")),
            Err(RouterError::NoProviderRegistered)
        ));
    }

    #[test]
    fn catalog_dispatch_picks_matching_provider() {
        let mut router = Router::new();
        router
            .register(StubProvider::new(
                ProviderName::DeepSeek,
                &["deepseek-v4-flash"],
            ))
            .register(StubProvider::new(ProviderName::Zhipu, &["glm-5.2"]));

        let provider = router.provider(&ModelId::from("glm-5.2")).unwrap();
        assert_eq!(provider.provider_name(), ProviderName::Zhipu);
    }

    #[test]
    fn first_registered_provider_wins_on_catalog_conflict() {
        let mut router = Router::new();
        router
            .register(StubProvider::new(ProviderName::DeepSeek, &["shared-model"]))
            .register(StubProvider::new(ProviderName::Zhipu, &["shared-model"]));

        let provider = router.provider(&ModelId::from("shared-model")).unwrap();
        assert_eq!(provider.provider_name(), ProviderName::DeepSeek);
    }

    #[test]
    fn unknown_model_is_error() {
        let mut router = Router::new();
        router.register(StubProvider::new(
            ProviderName::DeepSeek,
            &["deepseek-v4-flash"],
        ));

        let result = router.provider(&ModelId::from("xai/unknown"));
        assert!(matches!(
            result,
            Err(RouterError::UnknownModel(ref model)) if model.as_str() == "xai/unknown"
        ));
    }

    #[tokio::test]
    async fn complete_delegates_to_resolved_provider() {
        let mut router = Router::new();
        let deepseek = StubProvider::new(ProviderName::DeepSeek, &["deepseek-v4-flash"]);
        let calls = deepseek.calls();
        router.register(deepseek);

        let response = Client::from(router)
            .complete(&request("deepseek-v4-flash"))
            .await
            .unwrap();
        assert_eq!(response.model, "deepseek-v4-flash");
        assert_eq!(*calls.lock().unwrap(), vec![ProviderName::DeepSeek]);
    }

    #[tokio::test]
    async fn stream_delegates_to_resolved_provider() {
        let mut router = Router::new();
        let zhipu = StubProvider::new(ProviderName::Zhipu, &["glm-5.2"]);
        let calls = zhipu.calls();
        router.register(zhipu);

        let stream = Client::from(router)
            .stream(&request("glm-5.2"))
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Ok(StreamEvent::MessageStop)));
        assert_eq!(*calls.lock().unwrap(), vec![ProviderName::Zhipu]);
    }

    #[tokio::test]
    async fn provider_errors_pass_through_complete() {
        let mut router = Router::new();
        router.register(StubProvider::failing(
            ProviderName::DeepSeek,
            &["deepseek-v4-flash"],
        ));

        let err = Client::from(router)
            .complete(&request("deepseek-v4-flash"))
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::Auth(_)));
    }

    #[tokio::test]
    async fn stream_start_failure_is_an_error_not_a_stream() {
        let mut router = Router::new();
        router.register(StubProvider::failing(
            ProviderName::DeepSeek,
            &["deepseek-v4-flash"],
        ));

        let result = Client::from(router)
            .stream(&request("deepseek-v4-flash"))
            .await;
        assert!(matches!(result, Err(ProviderError::Auth(_))));
    }

    #[tokio::test]
    async fn stream_routing_failure_returns_err() {
        let mut router = Router::new();
        router.register(StubProvider::new(
            ProviderName::DeepSeek,
            &["deepseek-v4-flash"],
        ));

        let result = Client::from(router).stream(&request("xai/grok-4.6")).await;
        assert!(matches!(result, Err(ProviderError::UnknownModel(_))));
    }

    #[test]
    fn slug_vendor_dispatches_and_variant_picks_protocol() {
        let mut router = Router::new();
        router
            .register(
                StubProvider::new(ProviderName::DeepSeek, &["deepseek-v4-flash"])
                    .with_protocol(ProviderKind::Completions),
            )
            .register(
                StubProvider::new(ProviderName::DeepSeek, &["deepseek-v4-flash"])
                    .with_protocol(ProviderKind::Responses),
            )
            .register(
                StubProvider::new(ProviderName::Grok, &["grok-4.6"])
                    .with_protocol(ProviderKind::Responses),
            );

        assert_eq!(
            router
                .provider(&ModelId::from("deepseek/deepseek-v4-flash"))
                .unwrap()
                .protocol(),
            Some(ProviderKind::Completions)
        );
        assert_eq!(
            router
                .provider(&ModelId::from("deepseek/deepseek-v4-flash:responses"))
                .unwrap()
                .protocol(),
            Some(ProviderKind::Responses)
        );
        assert_eq!(
            router
                .provider(&ModelId::from("xai/grok-4.6"))
                .unwrap()
                .provider_name(),
            ProviderName::Grok
        );
        assert_eq!(
            router.qualify(&ModelId::from("grok/grok-4.6")).as_str(),
            "xai/grok-4.6"
        );
    }

    #[test]
    fn single_vendor_qualifies_bare_id() {
        let mut router = Router::new();
        router.register(StubProvider::new(ProviderName::Moonshot, &["kimi-k3"]));
        assert_eq!(
            router.qualify(&ModelId::from("kimi-k3")).as_str(),
            "moonshot/kimi-k3"
        );
        assert_eq!(
            router
                .provider(&ModelId::from("kimi-k3"))
                .unwrap()
                .provider_name(),
            ProviderName::Moonshot
        );
    }

    #[test]
    fn upsert_replaces_same_vendor_keeps_others() {
        let mut router = Router::new();
        router
            .register(StubProvider::new(ProviderName::DeepSeek, &["old-model"]))
            .register(StubProvider::new(ProviderName::Zhipu, &["glm-5.2"]));
        router.upsert(StubProvider::new(ProviderName::DeepSeek, &["new-model"]));

        let ids: Vec<_> = router
            .known_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert!(ids.iter().any(|id| id.contains("new-model")));
        assert!(!ids.iter().any(|id| id.contains("old-model")));
        assert!(ids.iter().any(|id| id.contains("glm-5.2")));
    }

    #[test]
    fn upsert_keeps_the_other_protocol_for_the_same_vendor() {
        let mut router = Router::new();
        router
            .register(
                StubProvider::new(ProviderName::DeepSeek, &["completions-model"])
                    .with_protocol(ProviderKind::Completions),
            )
            .register(
                StubProvider::new(ProviderName::DeepSeek, &["responses-model"])
                    .with_protocol(ProviderKind::Responses),
            );
        router.upsert(
            StubProvider::new(ProviderName::DeepSeek, &["replaced"])
                .with_protocol(ProviderKind::Completions),
        );

        let ids: Vec<_> = router
            .known_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert!(ids.iter().any(|id| id.contains("replaced")));
        assert!(!ids.iter().any(|id| id.contains("completions-model")));
        assert!(ids.iter().any(|id| id.contains("responses-model")));
        assert_eq!(
            router
                .provider(&ModelId::from("deepseek/responses-model:responses"))
                .unwrap()
                .protocol(),
            Some(ProviderKind::Responses)
        );
    }

    #[test]
    fn handle_replaces_the_snapshot() {
        let handle = RouterHandle::new(Router::new());
        let held = handle.load();
        assert!(matches!(
            held.provider(&ModelId::from("anything")),
            Err(RouterError::NoProviderRegistered)
        ));

        let mut next = Router::new();
        next.register(StubProvider::new(ProviderName::Zhipu, &["glm-5.2"]));
        handle.replace(next);

        assert!(matches!(
            held.provider(&ModelId::from("glm-5.2")),
            Err(RouterError::NoProviderRegistered)
        ));
        assert_eq!(
            handle
                .load()
                .provider(&ModelId::from("glm-5.2"))
                .unwrap()
                .provider_name(),
            ProviderName::Zhipu
        );
    }
}
