//! The official `rmcp` SDK over the existing [`Server`]: `ServerHandler` by hand, no `#[tool]`
//! (`docs/decisions/0252-the-cli-and-the-mcp-server-are-two-doors-over-one-client-and-every-hub-has-its-own-client.md`).
//!
//! # The shape: the SDK owns the protocol, one thread owns the [`Server`]
//!
//! rmcp runs EVERY request as a task of its own on `&self` with a `Send` future; the [`Server`] takes
//! `&mut self` and blocks for as long as a node answers. So nothing here shares it: the thread in
//! [`serve`](super::serve) is one serial loop, the runtime lives on a second thread, and each handler
//! sends a [`Job`] and awaits the reply on a oneshot. No mutex, and no field of the [`Server`] moved.
//! Everything keeps going through [`Server::handle`], so previews, the node handles, `last_backtest`,
//! the transcript's order, the scope gate and the panic guard are one code path, as they always were.
//!
//! ⚠ **Arrival order is [`Wire`](super::rmcp_wire::Wire)'s**: it hands rmcp one inline request at a
//! time, so a confirm cannot overtake its preview on any runtime.
//!
//! The three [`DEFERRED_TOOLS`](super::protocol::DEFERRED_TOOLS) keep their worker thread: the owned
//! [`DeferredCall`] goes out, the outcome comes back to the loop.
//!
//! # What the SDK answers by itself, and what it is not allowed to
//!
//! `server/discover`, `ping`, the 2026-07-28 `_meta` checks and the version negotiation are the SDK's:
//! that is what the swap is for. Three of its defaults would change an answer this server has always
//! given, so each is overridden back to `-32601`: `resources/templates/list`, `completion/complete`
//! and `logging/setLevel`. A capability declared is a promise; those three were never declared.
//!
//! # A panic never leaves a client waiting
//!
//! `Server::handle` and `run_deferred` catch every tool. What is left on this side is the SDK's own
//! code, and it runs in two places:
//!
//! - **A handler task**, whose `JoinHandle` the SDK drops, so a panic there would lose that answer and
//!   leave [`Wire`](super::rmcp_wire::Wire) waiting for it, reading no further line and outliving its
//!   client's EOF. [`Guarded`] turns it into that request's `-32603`, and the session goes on.
//! - **The SDK's own loop**, which no request owns. Its `JoinHandle` is the one `waiting` holds, so
//!   `serve` returns the failure and the process exits failing. That needs no guard.
//!
//! shortcut: the handler task's last lines, after `handle_request` returns (a constructor, a log line,
//! a channel send), are outside [`Guarded`], so a panic there still wedges the session. Upgrade if
//! rmcp ever gives a task hook.

use std::borrow::Cow;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::task::Poll;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientNotification, ClientRequest,
    CompleteRequestParams, CompleteResult, CustomRequest, CustomResult, ErrorCode,
    GetPromptRequestParams, GetPromptResponse, GetPromptResult, Implementation,
    InitializeRequestParams, InitializeResult, ListPromptsResult, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, ServerCapabilities,
    ServerConfig, ServerResult,
};
// Deprecated upstream (SEP-2577), and named only by the handler that refuses it.
#[expect(deprecated)]
use rmcp::model::SetLevelRequestParams;
use rmcp::service::{NotificationContext, RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, Service};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use super::cancel::CancelToken;
use super::instructions::instructions;
use super::protocol::{DeferredCall, DeferredOutcome, run_deferred};
use super::{SERVER_NAME, SERVER_VERSION, Server};

/// What a handler (or the runtime thread, or the wire) asks of the loop.
pub(super) enum Job {
    /// One JSON-RPC request: [`Server::classify`], then [`Server::handle`].
    Message { msg: Value, reply: oneshot::Sender<Answer> },
    /// A deferred call came back. `None` in the reply: it was cancelled, so nothing is to be sent.
    Finished {
        call: Box<DeferredCall>,
        outcome: DeferredOutcome,
        reply: oneshot::Sender<Option<Value>>,
    },
    /// Bytes for the writer, from the wire.
    Output(Vec<u8>),
    /// The session is over, and how.
    Ended(io::Result<()>),
}

pub(super) enum Answer {
    Inline(Option<Value>),
    Deferred(Box<DeferredCall>),
}

pub(super) struct Seam {
    pub(super) jobs: mpsc::Sender<Job>,
    pub(super) info: ServerConfig,
}

impl Seam {
    /// The whole JSON-RPC response the [`Server`] gives this request. Enqueued at once, on the
    /// handler's first poll: see the module doc's order paragraph.
    async fn answer(
        &self,
        ctx: &RequestContext<RoleServer>,
        method: &str,
        params: Value,
    ) -> Result<Value, ErrorData> {
        let (reply, answer) = oneshot::channel();
        let msg = json!({ "jsonrpc": "2.0", "id": ctx.id, "method": method, "params": params });
        let _ = self.jobs.send(Job::Message { msg, reply });
        match answer.await {
            Ok(Answer::Inline(Some(response))) => Ok(response),
            Ok(Answer::Deferred(call)) => self.deferred(call, ctx).await,
            _ => Err(gone()),
        }
    }

    /// [`Self::answer`] as the type the SDK will serialize again.
    async fn forward<T: DeserializeOwned>(
        &self,
        ctx: &RequestContext<RoleServer>,
        method: &str,
        params: Value,
    ) -> Result<T, ErrorData> {
        let mut result = result_of(&self.answer(ctx, method, params).await?)?;
        // 2026-07-28 requires it, and the SDK takes it off again for any older session.
        if let Some(object) = result.as_object_mut() {
            object.insert("resultType".to_string(), json!("complete"));
        }
        serde_json::from_value(result).map_err(|e| {
            ErrorData::internal_error(format!("the answer does not fit the SDK's type: {e}"), None)
        })
    }

    /// The worker half of a daemon RUN. `ctx.ct` is rmcp's per-request cancel: it closes the socket
    /// through the existing [`CancelToken`]. Nobody waits for a cancelled worker; its end only reaches
    /// the transcript.
    async fn deferred(
        &self,
        call: Box<DeferredCall>,
        ctx: &RequestContext<RoleServer>,
    ) -> Result<Value, ErrorData> {
        let token = CancelToken::new();
        let worker_token = token.clone();
        let (finished, mut worker) = oneshot::channel();
        std::thread::Builder::new()
            .name("mcp-tool".into())
            .spawn(move || {
                let outcome = run_deferred(&call, &worker_token);
                let _ = finished.send((call, outcome));
            })
            .map_err(|e| ErrorData::internal_error(format!("no thread for this run: {e}"), None))?;
        tokio::select! {
            joined = &mut worker => {
                let (call, outcome) = joined.map_err(|_| gone())?;
                let (reply, answer) = oneshot::channel();
                let _ = self.jobs.send(Job::Finished { call, outcome, reply });
                answer.await.ok().flatten().ok_or_else(gone)
            }
            () = ctx.ct.cancelled() => {
                token.cancel();
                let jobs = self.jobs.clone();
                tokio::spawn(async move {
                    if let Ok((call, _)) = worker.await {
                        let (reply, _) = oneshot::channel();
                        let outcome = DeferredOutcome::Cancelled;
                        let _ = jobs.send(Job::Finished { call, outcome, reply });
                    }
                });
                // rmcp drops this: the cancel already took the id out of its table.
                Err(gone())
            }
        }
    }
}

fn gone() -> ErrorData {
    ErrorData::internal_error("the request ended without an answer", None)
}

/// `-32601`, in the words [`Server::handle`] says it.
fn not_served(method: &str) -> ErrorData {
    ErrorData::new(ErrorCode::METHOD_NOT_FOUND, format!("method not found: {method}"), None)
}

/// The `result` of a whole JSON-RPC response, or its `error` as the SDK's.
fn result_of(response: &Value) -> Result<Value, ErrorData> {
    if let Some(error) = response.get("error") {
        let code = error["code"].as_i64().unwrap_or(-32603) as i32;
        let message = error["message"].as_str().unwrap_or_default().to_string();
        return Err(ErrorData::new(ErrorCode(code), message, None));
    }
    Ok(response["result"].clone())
}

impl ServerHandler for Seam {
    fn get_info(&self) -> ServerConfig {
        self.info.clone()
    }

    /// The SDK's negotiation, with the session recording what was AGREED. ⚠ Its default records what
    /// the client ASKED for, the handshake path aside, so a client answered `2025-11-25` to its
    /// `2026-07-28` would then lose `ping` and get the newer result shape in a session it reads as
    /// the older one.
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let agreed = self.negotiate_initialize(&request)?;
        let mut peer = request;
        peer.protocol_version = agreed.protocol_version.clone();
        ctx.peer.set_peer_info(peer);
        Ok(agreed)
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.forward(&ctx, "tools/list", json!({})).await
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = request.arguments.unwrap_or_default();
        let params = json!({ "name": request.name, "arguments": arguments });
        let result: CallToolResult = self.forward(&ctx, "tools/call", params).await?;
        Ok(result.into())
    }

    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        self.forward(&ctx, "resources/list", json!({})).await
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let params = json!({ "uri": request.uri });
        let result: ReadResourceResult = self.forward(&ctx, "resources/read", params).await?;
        Ok(result.into())
    }

    async fn list_prompts(
        &self,
        _: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        self.forward(&ctx, "prompts/list", json!({})).await
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let arguments = request.arguments.unwrap_or_default();
        let params = json!({ "name": request.name, "arguments": arguments });
        let result: GetPromptResult = self.forward(&ctx, "prompts/get", params).await?;
        Ok(result.into())
    }

    async fn list_resource_templates(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Err(not_served("resources/templates/list"))
    }

    async fn complete(
        &self,
        _: CompleteRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        Err(not_served("completion/complete"))
    }

    #[expect(deprecated)]
    async fn set_level(
        &self,
        _: SetLevelRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Err(not_served("logging/setLevel"))
    }

    /// An unknown method lands here, and so does a known one whose params do not fit the SDK's type:
    /// the [`Server`]'s answer is the one to give. `initialize` is not the [`Server`]'s, and the type
    /// wants a `clientInfo` and `capabilities` this server has never asked for: a client that names
    /// no version gets the oldest, one that names a version gets the negotiation.
    async fn on_custom_request(
        &self,
        request: CustomRequest,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CustomResult, ErrorData> {
        // A test-only method that panics on the SDK's path, past `Server::handle`'s catch_unwind.
        #[cfg(test)]
        if request.method == "__test_sdk_panic" {
            panic!("kaboom");
        }
        let params = request.params.unwrap_or_else(|| json!({}));
        if request.method == "initialize" {
            let named = serde_json::from_value(params["protocolVersion"].clone());
            let asked = InitializeRequestParams::default()
                .with_protocol_version(named.unwrap_or(ProtocolVersion::V_2024_11_05));
            let agreed = serde_json::to_value(self.negotiate_initialize(&asked)?)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            return Ok(CustomResult::new(agreed));
        }
        let response = self.answer(&ctx, &request.method, params).await?;
        result_of(&response).map(CustomResult::new)
    }
}

/// [`Seam`] as the SDK runs it: a request whose handler task panics is answered. The module doc's
/// last section has the reason; the panic's own line reaches stderr from the panic hook.
pub(super) struct Guarded(pub(super) Seam);

impl Service<RoleServer> for Guarded {
    async fn handle_request(
        &self,
        request: ClientRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<ServerResult, ErrorData> {
        let mut handler = std::pin::pin!(self.0.handle_request(request, context));
        std::future::poll_fn(|cx| {
            catch_unwind(AssertUnwindSafe(|| handler.as_mut().poll(cx))).unwrap_or_else(|_| {
                let why = "this request ended in a panic in the server, so its outcome is unknown: \
                           if it was a write, it may have gone out. Read the node before sending \
                           it again";
                Poll::Ready(Err(ErrorData::internal_error(why, None)))
            })
        })
        .await
    }

    async fn handle_notification(
        &self,
        notification: ClientNotification,
        context: NotificationContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.0.handle_notification(notification, context).await
    }

    fn get_info(&self) -> ServerConfig {
        ServerHandler::get_info(&self.0)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        ServerHandler::supported_protocol_versions(&self.0)
    }
}

/// What `initialize` and `server/discover` say about this session, before any negotiation.
pub(super) fn info_of(server: &Server) -> ServerConfig {
    // ⚠ A capability declared here is a PROMISE that the matching methods answer, and a client that
    // reads `resources` will call `resources/list` without being asked to.
    let capabilities =
        ServerCapabilities::builder().enable_tools().enable_resources().enable_prompts().build();
    ServerConfig::new(capabilities)
        .with_server_info(Implementation::new(SERVER_NAME, SERVER_VERSION))
        .with_instructions(instructions(&server.access))
}
