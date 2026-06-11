use crate::session_wrapper::SessionWrapper;
use anyhow::Result;
use aptos_types::account_address::AccountAddress;
use axum::{
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Json, Response},
    routing::{get, post},
    Router,
};
use move_core_types::identifier::Identifier;
use move_core_types::language_storage::{ModuleId, StructTag, TypeTag};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

type AppState = Arc<ServerState>;

/// JSON error in the canonical Aptos REST shape
/// (`{message, error_code, vm_error_code}`).
type ApiError = (StatusCode, Json<serde_json::Value>);

/// Local dev tool guardrails: one slow/hung VM call must not wedge every
/// other request (audit #7).
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_CONCURRENT_REQUESTS: usize = 64;

pub struct ServerOptions {
    pub auth_token: Option<String>,
    pub strict_local_auth: bool,
}

struct ServerState {
    session: SessionWrapper,
    options: ServerOptions,
}

pub async fn run(session: SessionWrapper, port: u16, options: ServerOptions) -> Result<()> {
    let state: AppState = Arc::new(ServerState { session, options });

    let v1 = Router::new()
        .route("/", get(ledger_info))
        .route("/accounts/:address", get(get_account))
        .route(
            "/accounts/:address/resource/*resource_type",
            get(get_account_resource),
        )
        .route("/accounts/:address/resources", get(get_account_resources))
        .route("/accounts/:address/module/:module_name", get(get_module))
        .route("/estimate_gas_price", get(estimate_gas_price))
        .route("/view", post(view_function))
        .route("/transactions", post(submit_transaction))
        .route("/transactions/simulate", post(simulate_transaction))
        .route("/transactions/trace", post(trace_transaction))
        .route("/transactions/by_hash/:hash", get(get_transaction_by_hash))
        .route(
            "/transactions/wait_by_hash/:hash",
            get(get_transaction_by_hash),
        );

    // Layer order (axum applies bottom-up, so the LAST .layer() is outermost):
    // ledger-header injection stays outermost so even timeout/limit error
    // responses carry the `x-aptos-*` headers the Aptos REST client requires.
    // HandleErrorLayer converts the timeout's BoxError into a response (Router
    // services must be infallible); putting Timeout above ConcurrencyLimit
    // makes the 30s budget also cover time spent queued on the semaphore.
    // GlobalConcurrencyLimitLayer (not ConcurrencyLimitLayer) shares one
    // semaphore across all connections. The timeout drops the response but
    // does NOT cancel the VM task — it runs to completion on the blocking
    // pool; real cancellation would need a VM actor (out of scope).
    let app = Router::new()
        .route("/v1/", get(ledger_info))
        .nest("/v1", v1)
        .route("/mint", post(mint))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(
            tower::ServiceBuilder::new()
                .layer(axum::error_handling::HandleErrorLayer::new(
                    handle_layer_error,
                ))
                .layer(tower::timeout::TimeoutLayer::new(REQUEST_TIMEOUT))
                .layer(tower::limit::GlobalConcurrencyLimitLayer::new(
                    MAX_CONCURRENT_REQUESTS,
                )),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            inject_ledger_headers,
        ))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await?;
    eprintln!("Listening on http://127.0.0.1:{}", port);
    axum::serve(listener, app).await?;

    Ok(())
}

#[derive(Serialize)]
struct LedgerInfoResponse {
    chain_id: u64,
    epoch: String,
    ledger_version: String,
    oldest_ledger_version: String,
    ledger_timestamp: String,
    node_role: String,
    oldest_block_height: String,
    block_height: String,
}

fn build_ledger_info(state: &ServerState) -> LedgerInfoResponse {
    let ops = state.session.get_ops_count();
    // Report real wall-clock microseconds. The Movement CLI derives a
    // transaction expiration from the ledger timestamp; a zero timestamp
    // makes that arithmetic underflow ("attempt to subtract with overflow").
    let now_usec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    LedgerInfoResponse {
        chain_id: state.session.get_chain_id(),
        epoch: "1".to_string(),
        ledger_version: ops.to_string(),
        oldest_ledger_version: "0".to_string(),
        ledger_timestamp: now_usec.to_string(),
        node_role: "full_node".to_string(),
        oldest_block_height: "0".to_string(),
        block_height: ops.to_string(),
    }
}

async fn ledger_info(State(session): State<AppState>) -> Json<LedgerInfoResponse> {
    Json(build_ledger_info(&session))
}

/// Attach the `X-Aptos-*` ledger-state headers to every response. The Aptos
/// REST client (used by the Movement CLI for `move publish` etc.) builds its
/// State from these response headers, not the JSON body; without them it fails
/// with "Failed to build State from headers". Computed after the handler runs
/// so the version reflects any state change (e.g. a committed transaction).
async fn inject_ledger_headers(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let mut response = next.run(req).await;
    let info = build_ledger_info(&state);
    let headers = response.headers_mut();
    let pairs = [
        ("x-aptos-chain-id", info.chain_id.to_string()),
        ("x-aptos-ledger-version", info.ledger_version),
        ("x-aptos-ledger-oldest-version", info.oldest_ledger_version),
        ("x-aptos-ledger-timestampusec", info.ledger_timestamp),
        ("x-aptos-epoch", info.epoch),
        ("x-aptos-block-height", info.block_height),
        ("x-aptos-oldest-block-height", info.oldest_block_height),
    ];
    for (name, value) in pairs {
        if let Ok(v) = HeaderValue::from_str(&value) {
            headers.insert(HeaderName::from_static(name), v);
        }
    }
    response
}

async fn estimate_gas_price() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "gas_estimate": 100,
        "deprioritized_gas_estimate": 100,
        "prioritized_gas_estimate": 150
    }))
}

#[derive(Serialize)]
struct AccountDataResponse {
    sequence_number: String,
    authentication_key: String,
}

/// Outcome of looking up `0x1::account::Account`, resolved on the blocking pool.
enum AccountLookup {
    Found(serde_json::Value),
    /// No Account resource, but `DEFAULT_ACCOUNT_RESOURCE` is enabled: a real
    /// node synthesizes a stateless account (`AccountResource::new_stateless`).
    Stateless,
    /// No Account resource and the feature is disabled (e.g. a fork of an
    /// older network): a real node returns 404.
    NotFound,
}

async fn get_account(
    State(session): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<AccountDataResponse>, ApiError> {
    let addr = parse_address(&address).map_err(to_json_error)?;

    let lookup = run_blocking(&session, move |s| -> anyhow::Result<AccountLookup> {
        let account_tag = StructTag {
            address: AccountAddress::ONE,
            module: Identifier::new("account").unwrap(),
            name: Identifier::new("Account").unwrap(),
            type_args: vec![],
        };
        match s.view_resource(addr, &account_tag)? {
            Some(value) => Ok(AccountLookup::Found(value)),
            None => {
                if s.is_default_account_resource_enabled()? {
                    Ok(AccountLookup::Stateless)
                } else {
                    Ok(AccountLookup::NotFound)
                }
            }
        }
    })
    .await
    .map_err(to_json_error)?
    .map_err(|e| {
        to_json_error((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("view_resource error for {}: {}", address, e),
        ))
    })?;

    let version = session.session.get_ops_count();
    match lookup {
        AccountLookup::Found(value) => {
            let seq = value
                .get("sequence_number")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .to_string();
            let auth_key = value
                .get("authentication_key")
                .and_then(|v| v.as_str())
                .unwrap_or("0x0")
                .to_string();
            Ok(Json(AccountDataResponse {
                sequence_number: seq,
                authentication_key: auth_key,
            }))
        }
        // Mirror `AccountResource::new_stateless`: the auth key is the
        // address itself, not zeros (audit #17).
        AccountLookup::Stateless => Ok(Json(AccountDataResponse {
            sequence_number: "0".to_string(),
            authentication_key: format!("0x{}", hex::encode(addr.to_vec())),
        })),
        AccountLookup::NotFound => Err(json_error(
            StatusCode::NOT_FOUND,
            "account_not_found",
            &format!(
                "Account not found by Address({}) and Ledger version({})",
                address, version
            ),
        )),
    }
}

#[derive(Serialize)]
struct ResourceResponse {
    r#type: String,
    data: serde_json::Value,
}

async fn get_account_resources(
    State(session): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<Vec<ResourceResponse>>, (StatusCode, String)> {
    let addr = parse_address(&address)?;

    // The session API does not expose a "list all resources" method.
    // We probe a fixed set of common framework types. User-deployed
    // resources require the specific GET /resource/:type endpoint.
    let known_types = [
        "0x1::account::Account",
        "0x1::coin::CoinStore<0x1::aptos_coin::AptosCoin>",
        "0x1::fungible_asset::FungibleStore",
        "0x1::fungible_asset::Metadata",
        "0x1::object::ObjectCore",
        "0x1::code::PackageRegistry",
        "0x1::staking_contract::Store",
    ];

    run_blocking(&session, move |s| {
        let mut resources = Vec::new();
        for type_str in &known_types {
            // Static, well-formed constants — a parse failure is a programmer
            // error, not a runtime condition.
            let tag = type_str
                .parse::<StructTag>()
                .expect("known_types entries are valid struct tags");
            // Distinguish "absent" (skip) from "storage failed" (propagate):
            // a partial 200 on an unhealthy session silently lies (audit #15).
            match s.view_resource(addr, &tag) {
                Ok(Some(data)) => resources.push(ResourceResponse {
                    r#type: type_str.to_string(),
                    data,
                }),
                Ok(None) => {}
                Err(e) => {
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("view_resource failed for {}: {}", type_str, e),
                    ))
                }
            }
        }
        Ok(resources)
    })
    .await?
    .map(Json)
}

async fn get_module(
    State(session): State<AppState>,
    Path((address, module_name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let addr = parse_address(&address)?;

    let bytes = {
        let module_name = module_name.clone();
        run_blocking(&session, move |s| s.get_module_bytes(addr, &module_name))
            .await?
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    };

    match bytes {
        Some(bytecode) => {
            let module_bytecode = aptos_api_types::MoveModuleBytecode::new(bytecode)
                .try_parse_abi()
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("ABI parse error: {}", e),
                    )
                })?;
            serde_json::to_value(module_bytecode)
                .map(Json)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        }
        None => Err((
            StatusCode::NOT_FOUND,
            format!("Module not found: {}::{}", address, module_name),
        )),
    }
}

async fn get_account_resource(
    State(session): State<AppState>,
    Path((address, resource_type)): Path<(String, String)>,
) -> Result<Json<ResourceResponse>, (StatusCode, String)> {
    let addr = parse_address(&address)?;
    let trimmed = resource_type.strip_prefix('/').unwrap_or(&resource_type);
    let decoded_type =
        urlencoding::decode(trimmed).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let tag = parse_struct_tag(&decoded_type)?;

    let lookup = {
        let tag = tag.clone();
        run_blocking(&session, move |s| s.view_resource(addr, &tag)).await?
    };
    match lookup {
        Ok(Some(data)) => Ok(Json(ResourceResponse {
            r#type: decoded_type.to_string(),
            data,
        })),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            format!("Resource not found: {}", decoded_type),
        )),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

#[derive(Deserialize)]
struct ViewRequest {
    function: String,
    type_arguments: Vec<String>,
    arguments: Vec<serde_json::Value>,
}

async fn view_function(
    State(session): State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");

    if content_type.contains("bcs") {
        let vf: aptos_api_types::ViewFunction = bcs::from_bytes(&body).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("BCS deserialize error: {}", e),
            )
        })?;

        return run_blocking(&session, move |s| {
            s.execute_view_function(vf.module, vf.function, vf.ty_args, vf.args)
        })
        .await?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()));
    }

    let payload: ViewRequest = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("JSON parse error: {}", e)))?;

    let (module_id, func_name) = parse_function_id(&payload.function)?;

    let ty_args: Vec<TypeTag> = payload
        .type_arguments
        .iter()
        .map(|s| parse_type_tag(s))
        .collect::<Result<_, _>>()?;

    let args: Vec<Vec<u8>> = payload
        .arguments
        .iter()
        .map(|v| serialize_view_arg(v))
        .collect::<Result<_, _>>()?;

    run_blocking(&session, move |s| {
        s.execute_view_function(module_id, func_name, ty_args, args)
    })
    .await?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

async fn submit_transaction(
    State(session): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_bcs_content_type(&headers)?;
    reject_bcs_accept(&headers)?;
    if session.options.strict_local_auth {
        require_auth(&headers, &session).map_err(to_json_error)?;
    }

    let txn: aptos_types::transaction::SignedTransaction = bcs::from_bytes(&body).map_err(|e| {
        to_json_error((
            StatusCode::BAD_REQUEST,
            format!("Failed to deserialize BCS transaction: {}", e),
        ))
    })?;

    let tx_hash = format!("0x{}", hex::encode(txn.committed_hash().to_vec()));
    let sender = format!("0x{}", hex::encode(txn.sender().to_vec()));
    let seq_num = txn.sequence_number().to_string();
    let max_gas = txn.max_gas_amount().to_string();
    let gas_price = txn.gas_unit_price().to_string();
    let expiration = txn.expiration_timestamp_secs().to_string();

    // The whole execute → bump version → record sequence runs inside one
    // blocking closure so a concurrent submit can't interleave between the
    // commit and its bookkeeping (audit #10).
    let record = {
        let tx_hash = tx_hash.clone();
        let sender = sender.clone();
        let seq_num = seq_num.clone();
        let max_gas = max_gas.clone();
        let gas_price = gas_price.clone();
        let expiration = expiration.clone();
        run_blocking(&session, move |s| -> anyhow::Result<()> {
            let (vm_status, output) = s.execute_transaction(txn)?;
            s.increment_ops();
            let version = s.get_ops_count().to_string();
            let success = vm_status == aptos_types::vm_status::VMStatus::Executed;
            let vm_status_str = if success {
                "Executed successfully".to_string()
            } else {
                format!("{:?}", vm_status)
            };
            let committed = serde_json::json!({
                "type": "user_transaction",
                "hash": tx_hash,
                "success": success,
                "vm_status": vm_status_str,
                "version": version,
                "sender": sender,
                "sequence_number": seq_num,
                "max_gas_amount": max_gas,
                "gas_unit_price": gas_price,
                "expiration_timestamp_secs": expiration,
                "gas_used": output.gas_used().to_string(),
                "timestamp": "0"
            });
            s.store_transaction(tx_hash, committed);
            Ok(())
        })
        .await
        .map_err(to_json_error)?
    };
    record.map_err(|e| {
        to_json_error((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    })?;

    Ok(Json(serde_json::json!({
        "type": "pending_transaction",
        "hash": tx_hash,
        "sender": sender,
        "sequence_number": seq_num,
        "max_gas_amount": max_gas,
        "gas_unit_price": gas_price,
        "expiration_timestamp_secs": expiration,
        "payload": {},
        "signature": {}
    })))
}

async fn get_transaction_by_hash(
    State(session): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    match session.session.get_transaction(&hash) {
        Some(tx) => Ok(Json(tx)),
        None => Err((
            StatusCode::NOT_FOUND,
            format!("Transaction not found: {}", hash),
        )),
    }
}

async fn simulate_transaction(
    State(session): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_bcs_content_type(&headers)?;
    reject_bcs_accept(&headers)?;

    let txn: aptos_types::transaction::SignedTransaction = bcs::from_bytes(&body).map_err(|e| {
        to_json_error((
            StatusCode::BAD_REQUEST,
            format!("Failed to deserialize BCS transaction: {}", e),
        ))
    })?;

    let tx_hash = format!("0x{}", hex::encode(txn.committed_hash().to_vec()));

    let (vm_status, output) = run_blocking(&session, move |s| s.simulate_transaction(txn))
        .await
        .map_err(to_json_error)?
        .map_err(|e| to_json_error((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())))?;

    let success = vm_status == aptos_types::vm_status::VMStatus::Executed;

    Ok(Json(vec![serde_json::json!({
        "hash": tx_hash,
        "vm_status": format!("{:?}", vm_status),
        "success": success,
        "gas_used": output.gas_used().to_string(),
    })]))
}

#[derive(Deserialize)]
struct TraceParams {
    /// When true, also commit the transaction (single-pass trace + submit).
    /// Defaults to false (simulate-like, read-only).
    commit: Option<bool>,
}

/// Opt-in Foundry-style execution trace. Accepts a BCS-signed transaction just
/// like submit/simulate and executes it through the instrumented VM path,
/// returning the call tree. With `?commit=true` it also commits the result in a
/// single pass (and is auth-gated like submit). The normal submit/simulate paths
/// do not pay the tracing overhead.
async fn trace_transaction(
    State(session): State<AppState>,
    Query(params): Query<TraceParams>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let commit = params.commit.unwrap_or(false);
    require_bcs_content_type(&headers)?;
    reject_bcs_accept(&headers)?;

    let txn: aptos_types::transaction::SignedTransaction = bcs::from_bytes(&body).map_err(|e| {
        to_json_error((
            StatusCode::BAD_REQUEST,
            format!("Failed to deserialize BCS transaction: {}", e),
        ))
    })?;

    // Committing mutates state, so gate it like submit_transaction.
    if commit && session.options.strict_local_auth {
        require_auth(&headers, &session).map_err(to_json_error)?;
    }

    let trace = run_blocking(&session, move |s| s.execute_transaction_traced(txn, commit))
        .await
        .map_err(to_json_error)?
        .map_err(|e| to_json_error((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())))?;

    serde_json::to_value(trace)
        .map(Json)
        .map_err(|e| to_json_error((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())))
}

#[derive(Deserialize)]
struct MintQuery {
    address: String,
    amount: u64,
}

async fn mint(
    State(session): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<MintQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    require_auth(&headers, &session)?;

    let addr = parse_address(&params.address)?;

    let amount = params.amount;
    run_blocking(&session, move |s| -> anyhow::Result<()> {
        s.fund_account(addr, amount)?;
        s.increment_ops();
        Ok(())
    })
    .await?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "address": params.address,
        "amount": params.amount
    })))
}

// --- helpers ---

/// Runs VM/session work on the blocking thread pool so it never stalls a tokio
/// worker (the session mutex is held for the whole VM execution). A panic in
/// the closure surfaces as a 500 on THIS request only — combined with the
/// non-poisoning session lock, it can't brick subsequent requests (audit #7/#11).
async fn run_blocking<R, F>(state: &AppState, f: F) -> Result<R, (StatusCode, String)>
where
    F: FnOnce(&SessionWrapper) -> R + Send + 'static,
    R: Send + 'static,
{
    let state = state.clone();
    tokio::task::spawn_blocking(move || f(&state.session))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("VM task panicked: {e}"),
            )
        })
}

/// Canonical Aptos REST error body.
fn json_error(status: StatusCode, error_code: &str, message: &str) -> ApiError {
    (
        status,
        Json(serde_json::json!({
            "message": message,
            "error_code": error_code,
            "vm_error_code": null
        })),
    )
}

/// Converts the plain-text error tuple used by most handlers into the
/// canonical JSON error shape.
fn to_json_error((status, message): (StatusCode, String)) -> ApiError {
    let code = match status {
        StatusCode::BAD_REQUEST => "invalid_input",
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::NOT_FOUND => "not_found",
        _ => "internal_error",
    };
    json_error(status, code, &message)
}

/// BCS transaction endpoints reject JSON bodies with 415 instead of a
/// confusing BCS deserialize error (audit #12). A missing Content-Type is
/// allowed for raw-client compatibility — the body is still BCS-validated.
fn require_bcs_content_type(headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(content_type) = headers.get("content-type").and_then(|v| v.to_str().ok()) else {
        return Ok(());
    };
    // Lenient sniff, same as /v1/view: matches the SDK's canonical
    // `application/x.aptos.signed_transaction+bcs` and generic
    // `application/x-bcs`. `application/octet-stream` is also tolerated.
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("bcs") || ct.contains("octet-stream") {
        return Ok(());
    }
    Err(json_error(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
        "movelite only accepts BCS-signed transactions; set Content-Type: \
         application/x.aptos.signed_transaction+bcs (JSON transaction submission is not supported)",
    ))
}

/// movelite only emits JSON. If the client's Accept header demands a BCS
/// response and admits nothing else, fail loudly with 406 instead of
/// returning a body it won't parse (audit #13).
fn reject_bcs_accept(headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(accept) = headers.get("accept").and_then(|v| v.to_str().ok()) else {
        return Ok(());
    };
    let accept = accept.to_ascii_lowercase();
    let demands_bcs = accept.contains("bcs");
    let admits_json = accept.contains("application/json") || accept.contains("*/*");
    if demands_bcs && !admits_json {
        return Err(json_error(
            StatusCode::NOT_ACCEPTABLE,
            "not_acceptable",
            "movelite only produces JSON responses; BCS response encoding is not supported",
        ));
    }
    Ok(())
}

/// Converts tower-layer errors (the Router itself must be infallible).
async fn handle_layer_error(err: tower::BoxError) -> (StatusCode, String) {
    if err.is::<tower::timeout::error::Elapsed>() {
        (
            StatusCode::REQUEST_TIMEOUT,
            format!("request timed out after {}s", REQUEST_TIMEOUT.as_secs()),
        )
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("service error: {err}"),
        )
    }
}

fn serialize_view_arg(v: &serde_json::Value) -> Result<Vec<u8>, (StatusCode, String)> {
    match v {
        serde_json::Value::String(s) => {
            if let Ok(addr) =
                AccountAddress::from_hex_literal(s).or_else(|_| AccountAddress::from_hex(s))
            {
                bcs::to_bytes(&addr)
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("BCS error: {}", e)))
            } else {
                bcs::to_bytes(s).map_err(|e| (StatusCode::BAD_REQUEST, format!("BCS error: {}", e)))
            }
        }
        serde_json::Value::Number(n) => {
            if let Some(val) = n.as_u64() {
                bcs::to_bytes(&val)
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("BCS error: {}", e)))
            } else {
                Err((
                    StatusCode::BAD_REQUEST,
                    format!("Unsupported number: {}", n),
                ))
            }
        }
        serde_json::Value::Bool(b) => {
            bcs::to_bytes(b).map_err(|e| (StatusCode::BAD_REQUEST, format!("BCS error: {}", e)))
        }
        _ => Err((
            StatusCode::BAD_REQUEST,
            format!("Unsupported arg type: {}", v),
        )),
    }
}

fn parse_address(s: &str) -> Result<AccountAddress, (StatusCode, String)> {
    AccountAddress::from_hex_literal(s)
        .or_else(|_| AccountAddress::from_hex(s))
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid address: {}", e)))
}

fn parse_function_id(s: &str) -> Result<(ModuleId, Identifier), (StatusCode, String)> {
    let parts: Vec<&str> = s.rsplitn(2, "::").collect();
    if parts.len() != 2 {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Invalid function ID: {}", s),
        ));
    }
    let func_name =
        Identifier::new(parts[0]).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let module_parts: Vec<&str> = parts[1].rsplitn(2, "::").collect();
    if module_parts.len() != 2 {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Invalid module ID in: {}", s),
        ));
    }
    let module_name =
        Identifier::new(module_parts[0]).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let address = parse_address(module_parts[1])?;

    Ok((ModuleId::new(address, module_name), func_name))
}

fn parse_struct_tag(s: &str) -> Result<StructTag, (StatusCode, String)> {
    let tag: StructTag = s
        .parse()
        .map_err(|e: anyhow::Error| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(tag)
}

fn parse_type_tag(s: &str) -> Result<TypeTag, (StatusCode, String)> {
    let tag: TypeTag = s
        .parse()
        .map_err(|e: anyhow::Error| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(tag)
}

fn require_auth(headers: &HeaderMap, state: &ServerState) -> Result<(), (StatusCode, String)> {
    let Some(expected) = &state.options.auth_token else {
        return Ok(());
    };

    let provided = headers.get("x-movelite-token").and_then(|v| v.to_str().ok());

    if provided == Some(expected.as_str()) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            "missing or invalid x-movelite-token".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        map
    }

    #[test]
    fn content_type_sdk_bcs_passes() {
        let h = headers(&[(
            "content-type",
            "application/x.aptos.signed_transaction+bcs",
        )]);
        assert!(require_bcs_content_type(&h).is_ok());
    }

    #[test]
    fn content_type_missing_passes() {
        assert!(require_bcs_content_type(&HeaderMap::new()).is_ok());
    }

    #[test]
    fn content_type_json_is_415() {
        let h = headers(&[("content-type", "application/json")]);
        let (status, _) = require_bcs_content_type(&h).unwrap_err();
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[test]
    fn accept_bcs_only_is_406() {
        let h = headers(&[("accept", "application/x-bcs")]);
        let (status, _) = reject_bcs_accept(&h).unwrap_err();
        assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
    }

    #[test]
    fn accept_json_or_wildcard_passes() {
        assert!(reject_bcs_accept(&headers(&[("accept", "application/json")])).is_ok());
        assert!(reject_bcs_accept(&headers(&[("accept", "*/*")])).is_ok());
        // BCS preferred but JSON admitted -> we can still answer.
        assert!(
            reject_bcs_accept(&headers(&[("accept", "application/x-bcs, application/json")]))
                .is_ok()
        );
        assert!(reject_bcs_accept(&HeaderMap::new()).is_ok());
    }
}
