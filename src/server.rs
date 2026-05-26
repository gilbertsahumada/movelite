use crate::session_wrapper::SessionWrapper;
use anyhow::Result;
use aptos_types::account_address::AccountAddress;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use move_core_types::identifier::Identifier;
use move_core_types::language_storage::{ModuleId, StructTag, TypeTag};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

type AppState = Arc<SessionWrapper>;

pub async fn run(session: SessionWrapper, port: u16) -> Result<()> {
    let state: AppState = Arc::new(session);

    let app = Router::new()
        .route("/v1/", get(ledger_info))
        .route("/v1/accounts/:address", get(get_account))
        .route(
            "/v1/accounts/:address/resource/*resource_type",
            get(get_account_resource),
        )
        .route("/v1/accounts/:address/resources", get(get_account_resources))
        .route("/v1/view", post(view_function))
        .route("/mint", post(mint))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await?;
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

async fn ledger_info(State(session): State<AppState>) -> Json<LedgerInfoResponse> {
    let ops = session.get_ops_count();
    Json(LedgerInfoResponse {
        chain_id: session.get_chain_id(),
        epoch: "1".to_string(),
        ledger_version: ops.to_string(),
        oldest_ledger_version: "0".to_string(),
        ledger_timestamp: "0".to_string(),
        node_role: "full_node".to_string(),
        oldest_block_height: "0".to_string(),
        block_height: ops.to_string(),
    })
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

    match session.view_resource(addr, &account_tag) {
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
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            format!("Account {} not found (view_resource returned None)", address),
        )),
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

    let known_types = [
        "0x1::account::Account",
        "0x1::coin::CoinStore<0x1::aptos_coin::AptosCoin>",
        "0x1::fungible_asset::FungibleStore",
        "0x1::object::ObjectCore",
    ];

    let mut resources = Vec::new();
    for type_str in &known_types {
        if let Ok(tag) = type_str.parse::<StructTag>() {
            if let Ok(Some(data)) = session.view_resource(addr, &tag) {
                resources.push(ResourceResponse {
                    r#type: type_str.to_string(),
                    data,
                });
            }
        }
    }

    Ok(Json(resources))
}

async fn get_account_resource(
    State(session): State<AppState>,
    Path((address, resource_type)): Path<(String, String)>,
) -> Result<Json<ResourceResponse>, (StatusCode, String)> {
    let addr = parse_address(&address)?;
    let trimmed = resource_type.strip_prefix('/').unwrap_or(&resource_type);
    let decoded_type = urlencoding::decode(trimmed)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let tag = parse_struct_tag(&decoded_type)?;

    match session.view_resource(addr, &tag) {
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
    Json(payload): Json<ViewRequest>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
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
        .execute_view_function(module_id, func_name, ty_args, args)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

#[derive(Deserialize)]
struct MintQuery {
    address: String,
    amount: u64,
}

async fn mint(
    State(session): State<AppState>,
    Query(params): Query<MintQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let addr = parse_address(&params.address)?;

    session
        .fund_account(addr, params.amount)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    session.increment_ops();

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
            if let Ok(addr) = AccountAddress::from_hex_literal(s)
                .or_else(|_| AccountAddress::from_hex(s))
            {
                bcs::to_bytes(&addr)
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("BCS error: {}", e)))
            } else {
                bcs::to_bytes(s)
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("BCS error: {}", e)))
            }
        }
        serde_json::Value::Number(n) => {
            if let Some(val) = n.as_u64() {
                bcs::to_bytes(&val)
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("BCS error: {}", e)))
            } else {
                Err((StatusCode::BAD_REQUEST, format!("Unsupported number: {}", n)))
            }
        }
        serde_json::Value::Bool(b) => {
            bcs::to_bytes(b)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("BCS error: {}", e)))
        }
        _ => Err((StatusCode::BAD_REQUEST, format!("Unsupported arg type: {}", v))),
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
    let tag: StructTag =
        s.parse().map_err(|e: anyhow::Error| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(tag)
}

fn parse_type_tag(s: &str) -> Result<TypeTag, (StatusCode, String)> {
    let tag: TypeTag =
        s.parse().map_err(|e: anyhow::Error| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(tag)
}
