use crate::ws_counter::ConnectionGuard;
use jsonrpsee::server::middleware::rpc::{Batch, Notification, Request, RpcServiceT};
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;
use tracing::Level;

#[derive(Clone)]
pub struct RpcTimingMiddleware<S> {
    pub service: S,
    pub level: Level,
    /// 每条 WS 连接持有一份，连接断开（所有 clone 析构）时自动 -1
    _conn_guard: Arc<ConnectionGuard>,
}

fn log_with_level(level: Level, method: &str, kind: &str, elapsed_us: u128, extra: &str) {
    match level {
        Level::ERROR => {
            tracing::error!(target: "rpc", rpc_kind = kind, method = method, elapsed_us = elapsed_us, "{extra}");
        }
        Level::WARN => {
            tracing::warn!(target: "rpc", rpc_kind = kind, method = method, elapsed_us = elapsed_us, "{extra}");
        }
        Level::INFO => {
            tracing::info!(target: "rpc", rpc_kind = kind, method = method, elapsed_us = elapsed_us, "{extra}");
        }
        Level::DEBUG => {
            tracing::debug!(target: "rpc", rpc_kind = kind, method = method, elapsed_us = elapsed_us, "{extra}");
        }
        Level::TRACE => {
            tracing::trace!(target: "rpc", rpc_kind = kind, method = method, elapsed_us = elapsed_us, "{extra}");
        }
    }
}

impl<S> RpcServiceT for RpcTimingMiddleware<S>
where
    S: RpcServiceT + Send + Sync + Clone + 'static,
{
    type MethodResponse = S::MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(
        &self,
        request: Request<'a>,
    ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let method_name = request.method_name().to_owned();
        let request_id = format!("{:?}", request.id());
        let level = self.level;
        let service = self.service.clone();
        let started_at = Instant::now();

        async move {
            let response = service.call(request).await;
            let elapsed_us = started_at.elapsed().as_micros();
            log_with_level(
                level,
                &method_name,
                "call",
                elapsed_us,
                &format!("rpc.call completed id={request_id}"),
            );
            response
        }
    }

    fn batch<'a>(&self, batch: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        let batch_size = batch.len();
        let mut method_names = Vec::with_capacity(batch_size);
        for entry in batch.iter() {
            match entry {
                Ok(item) => method_names.push(item.method_name().to_owned()),
                Err(_) => method_names.push("<invalid>".to_owned()),
            }
        }
        let methods = if method_names.is_empty() {
            "<empty>".to_owned()
        } else {
            method_names.join(",")
        };

        let level = self.level;
        let service = self.service.clone();
        let started_at = Instant::now();

        async move {
            let response = service.batch(batch).await;
            let elapsed_us = started_at.elapsed().as_micros();
            log_with_level(
                level,
                &methods,
                "batch",
                elapsed_us,
                &format!("rpc.batch completed size={batch_size}"),
            );
            response
        }
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        let method_name = n.method_name().to_owned();
        let level = self.level;
        let service = self.service.clone();
        let started_at = Instant::now();

        async move {
            let response = service.notification(n).await;
            let elapsed_us = started_at.elapsed().as_micros();
            log_with_level(
                level,
                &method_name,
                "notification",
                elapsed_us,
                "rpc.notification completed",
            );
            response
        }
    }
}
