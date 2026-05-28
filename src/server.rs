use crate::session_wrapper::SessionWrapper;
use anyhow::Result;
use aptos_types::account_address::AccountAddress;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use move_core_types::identifier::Identifier;
use move_core_types::language_storage::{ModuleId, StructTag, TypeTag};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

type AppState = Arc<ServerState>;

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
        .route("/transactions/by_hash/:hash", get(get_transaction_by_hash))
        .route(
            "/transactions/wait_by_hash/:hash",
            get(get_transaction_by_hash),
        );

    let app = Router::new()
        .route("/v1/", get(ledger_info))
        .nest("/v1", v1)
        .route("/mint", post(mint))
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
    LedgerInfoResponse {
        chain_id: state.session.get_chain_id(),
        epoch: "1".to_string(),
        ledger_version: ops.to_string(),
        oldest_ledger_version: "0".to_string(),
        ledger_timestamp: "0".to_string(),
        node_role: "full_node".to_string(),
        oldest_block_height: "0".to_string(),
        block_height: ops.to_string(),
    }
}

async fn ledger_info(State(session): State<AppState>) -> Json<LedgerInfoResponse> {
    Json(build_ledger_info(&session))
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

async fn get_account(
    State(session): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<AccountDataResponse>, (StatusCode, String)> {
    let addr = parse_address(&address)?;

    let account_tag = StructTag {
        address: AccountAddress::ONE,
        module: Identifier::new("account").unwrap(),
        name: Identifier::new("Account").unwrap(),
        type_args: vec![],
    };

    match session.session.view_resource(addr, &account_tag) {
        Ok(Some(value)) => {
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
        Ok(None) => Ok(Json(AccountDataResponse {
            sequence_number: "0".to_string(),
            authentication_key: format!("0x{}", "0".repeat(64)),
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("view_resource error for {}: {}", address, e),
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

    let mut resources = Vec::new();
    for type_str in &known_types {
        if let Ok(tag) = type_str.parse::<StructTag>() {
            if let Ok(Some(data)) = session.session.view_resource(addr, &tag) {
                resources.push(ResourceResponse {
                    r#type: type_str.to_string(),
                    data,
                });
            }
        }
    }

    Ok(Json(resources))
}

async fn get_module(
    State(session): State<AppState>,
    Path((address, module_name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let addr = parse_address(&address)?;

    let bytes = session
        .session
        .get_module_bytes(addr, &module_name)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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

    match session.session.view_resource(addr, &tag) {
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

        return session
            .session
            .execute_view_function(vf.module, vf.function, vf.ty_args, vf.args)
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

    session
        .session
        .execute_view_function(module_id, func_name, ty_args, args)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

async fn submit_transaction(
    State(session): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if session.options.strict_local_auth {
        require_auth(&headers, &session)?;
    }

    let txn: aptos_types::transaction::SignedTransaction = bcs::from_bytes(&body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Failed to deserialize BCS transaction: {}", e),
        )
    })?;

    let tx_hash = format!("0x{}", hex::encode(txn.committed_hash().to_vec()));
    let sender = format!("0x{}", hex::encode(txn.sender().to_vec()));
    let seq_num = txn.sequence_number().to_string();
    let max_gas = txn.max_gas_amount().to_string();
    let gas_price = txn.gas_unit_price().to_string();
    let expiration = txn.expiration_timestamp_secs().to_string();

    let (vm_status, output) = session
        .session
        .execute_transaction(txn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    session.session.increment_ops();
    let version = session.session.get_ops_count().to_string();
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

    session
        .session
        .store_transaction(tx_hash.clone(), committed);

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
    body: axum::body::Bytes,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let txn: aptos_types::transaction::SignedTransaction = bcs::from_bytes(&body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Failed to deserialize BCS transaction: {}", e),
        )
    })?;

    let tx_hash = format!("0x{}", hex::encode(txn.committed_hash().to_vec()));

    let (vm_status, output) = session
        .session
        .simulate_transaction(txn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let success = vm_status == aptos_types::vm_status::VMStatus::Executed;

    Ok(Json(vec![serde_json::json!({
        "hash": tx_hash,
        "vm_status": format!("{:?}", vm_status),
        "success": success,
        "gas_used": output.gas_used().to_string(),
    })]))
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

    session
        .session
        .fund_account(addr, params.amount)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    session.session.increment_ops();

    Ok(Json(serde_json::json!({
        "status": "ok",
        "address": params.address,
        "amount": params.amount
    })))
}

// --- helpers ---

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

    let provided = headers.get("x-mvlite-token").and_then(|v| v.to_str().ok());

    if provided == Some(expected.as_str()) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            "missing or invalid x-mvlite-token".to_string(),
        ))
    }
}
