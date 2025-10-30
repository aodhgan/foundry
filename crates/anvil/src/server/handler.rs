//! Contains RPC handlers
use crate::{
    EthApi,
    eth::error::to_rpc_result,
    pubsub::{EthSubscription, LogsSubscription},
};
use alloy_rpc_types::{
    FilteredParams,
    pubsub::{Params, SubscriptionKind},
};
use anvil_core::eth::{EthPubSub, EthRequest, EthRpcCall, subscription::SubscriptionId};
use anvil_rpc::{error::RpcError, response::ResponseResult};
use anvil_server::{PubSubContext, PubSubRpcHandler, RpcHandler};
use std::fmt;

#[derive(Clone)]
struct JsonRpcRequest<T> {
    parsed: T,
    raw: serde_json::Value,
}

impl<T> JsonRpcRequest<T> {
    fn into_parts(self) -> (T, serde_json::Value) {
        (self.parsed, self.raw)
    }
}

impl<T> fmt::Debug for JsonRpcRequest<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsonRpcRequest")
            .field("parsed", &self.parsed)
            .field("raw", &self.raw)
            .finish()
    }
}

impl<'de, T> serde::Deserialize<'de> for JsonRpcRequest<T>
where
    T: serde::de::DeserializeOwned,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let parsed = serde_json::from_value(raw.clone()).map_err(serde::de::Error::custom)?;
        Ok(Self { parsed, raw })
    }
}

/// A `RpcHandler` that expects `EthRequest` rpc calls via http
#[derive(Clone)]
pub struct HttpEthRpcHandler {
    /// Access to the node
    api: EthApi,
}

impl HttpEthRpcHandler {
    /// Creates a new instance of the handler using the given `EthApi`
    pub fn new(api: EthApi) -> Self {
        Self { api }
    }
}

#[async_trait::async_trait]
impl RpcHandler for HttpEthRpcHandler {
    type Request = JsonRpcRequest<EthRequest>;

    async fn on_request(&self, request: Self::Request) -> ResponseResult {
        let (request, raw) = request.into_parts();
        self.api.execute_with_raw(request, Some(raw)).await
    }
}

/// A `RpcHandler` that expects `EthRequest` rpc calls and `EthPubSub` via pubsub connection
#[derive(Clone)]
pub struct PubSubEthRpcHandler {
    /// Access to the node
    api: EthApi,
}

impl PubSubEthRpcHandler {
    /// Creates a new instance of the handler using the given `EthApi`
    pub fn new(api: EthApi) -> Self {
        Self { api }
    }

    /// Invoked for an ethereum pubsub rpc call
    async fn on_pub_sub(&self, pubsub: EthPubSub, cx: PubSubContext<Self>) -> ResponseResult {
        let id = SubscriptionId::random_hex();
        trace!(target: "rpc::ws", "received pubsub request {:?}", pubsub);
        match pubsub {
            EthPubSub::EthUnSubscribe(id) => {
                trace!(target: "rpc::ws", "canceling subscription {:?}", id);
                let canceled = cx.remove_subscription(&id).is_some();
                ResponseResult::Success(canceled.into())
            }
            EthPubSub::EthSubscribe(kind, raw_params) => {
                let filter = match &*raw_params {
                    Params::None => None,
                    Params::Logs(filter) => Some(filter.clone()),
                    Params::Bool(_) => None,
                };
                let params = FilteredParams::new(filter.map(|b| *b));

                let subscription = match kind {
                    SubscriptionKind::Logs => {
                        if raw_params.is_bool() {
                            return ResponseResult::Error(RpcError::invalid_params(
                                "Expected params for logs subscription",
                            ));
                        }

                        trace!(target: "rpc::ws", "received logs subscription {:?}", params);
                        let blocks = self.api.new_block_notifications();
                        let storage = self.api.storage_info();
                        EthSubscription::Logs(Box::new(LogsSubscription {
                            blocks,
                            storage,
                            filter: params,
                            queued: Default::default(),
                            id: id.clone(),
                        }))
                    }
                    SubscriptionKind::NewHeads => {
                        trace!(target: "rpc::ws", "received header subscription");
                        let blocks = self.api.new_block_notifications();
                        let storage = self.api.storage_info();
                        EthSubscription::Header(blocks, storage, id.clone())
                    }
                    SubscriptionKind::NewPendingTransactions => {
                        trace!(target: "rpc::ws", "received pending transactions subscription");
                        match *raw_params {
                            Params::Bool(true) => EthSubscription::FullPendingTransactions(
                                self.api.full_pending_transactions(),
                                id.clone(),
                            ),
                            Params::Bool(false) | Params::None => {
                                EthSubscription::PendingTransactions(
                                    self.api.new_ready_transactions(),
                                    id.clone(),
                                )
                            }
                            _ => {
                                return ResponseResult::Error(RpcError::invalid_params(
                                    "Expected boolean parameter for newPendingTransactions",
                                ));
                            }
                        }
                    }
                    SubscriptionKind::Syncing => {
                        return RpcError::internal_error_with("Not implemented").into();
                    }
                };

                cx.add_subscription(id.clone(), subscription);

                trace!(target: "rpc::ws", "created new subscription: {:?}", id);
                to_rpc_result(id)
            }
        }
    }
}

#[async_trait::async_trait]
impl PubSubRpcHandler for PubSubEthRpcHandler {
    type Request = EthRpcCall;
    type SubscriptionId = SubscriptionId;
    type Subscription = EthSubscription;

    async fn on_request(&self, request: Self::Request, cx: PubSubContext<Self>) -> ResponseResult {
        trace!(target: "rpc", "received pubsub request {:?}", request);
        match request {
            EthRpcCall::Request(request) => self.api.execute(*request).await,
            EthRpcCall::PubSub(pubsub) => self.on_pub_sub(pubsub, cx).await,
        }
    }
}
