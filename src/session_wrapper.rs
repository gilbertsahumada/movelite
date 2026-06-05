use crate::trace::{build_response, TraceResponse, TracerMeter};
use anyhow::Result;
use aptos_transaction_simulation::SimulationStateStore;
use aptos_transaction_simulation_session::Session;
use aptos_types::account_address::AccountAddress;
use aptos_types::chain_id::ChainId;
use aptos_types::state_store::state_key::StateKey;
use aptos_types::state_store::TStateView;
use aptos_types::transaction::{
    AuxiliaryInfo, EntryFunction, PersistedAuxiliaryInfo, SignedTransaction, TransactionExecutable,
    TransactionOutput, TransactionPayload, TransactionPayloadInner,
};
use aptos_types::vm_status::VMStatus;
use aptos_vm::{data_cache::AsMoveResolver, AptosVM};
use aptos_vm_environment::environment::AptosEnvironment;
use aptos_vm_logging::log_schema::AdapterLogSchema;
use aptos_vm_types::module_and_script_storage::AsAptosCodeStorage;
use move_core_types::identifier::Identifier;
use move_core_types::language_storage::{ModuleId, StructTag, TypeTag};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct SessionOptions {
    pub fork_url: Option<String>,
    pub fork_version: Option<u64>,
    pub chain_id: Option<u8>,
    pub session_dir: Option<PathBuf>,
    pub reset: bool,
}

pub struct SessionWrapper {
    inner: Mutex<Session>,
    ops_count: Mutex<u64>,
    tx_store: Mutex<HashMap<String, serde_json::Value>>,
    session_path: PathBuf,
    chain_id: u64,
}

impl SessionWrapper {
    pub fn new(session: Session, session_path: PathBuf, chain_id: u64) -> Self {
        Self {
            inner: Mutex::new(session),
            ops_count: Mutex::new(0),
            tx_store: Mutex::new(HashMap::new()),
            session_path,
            chain_id,
        }
    }

    pub fn fund_account(&self, address: AccountAddress, amount: u64) -> Result<()> {
        let mut session = self.inner.lock().unwrap();
        session.fund_account(address, amount)
    }

    pub fn execute_view_function(
        &self,
        module_id: ModuleId,
        function_name: Identifier,
        ty_args: Vec<TypeTag>,
        args: Vec<Vec<u8>>,
    ) -> Result<Vec<serde_json::Value>> {
        let mut session = self.inner.lock().unwrap();
        session.execute_view_function(module_id, function_name, ty_args, args)
    }

    pub fn view_resource(
        &self,
        account_addr: AccountAddress,
        resource_tag: &StructTag,
    ) -> Result<Option<serde_json::Value>> {
        let mut session = self.inner.lock().unwrap();
        session.view_resource(account_addr, resource_tag)
    }

    pub fn execute_transaction(
        &self,
        txn: SignedTransaction,
    ) -> Result<(VMStatus, TransactionOutput)> {
        let mut session = self.inner.lock().unwrap();
        session.execute_transaction(txn)
    }

    pub fn simulate_transaction(
        &self,
        txn: SignedTransaction,
    ) -> Result<(VMStatus, TransactionOutput)> {
        let temp_path = make_temp_session_path("simulate")?;
        std::fs::create_dir_all(&temp_path)?;

        let result = (|| {
            {
                let _session_guard = self.inner.lock().unwrap();
                copy_session_file(&self.session_path, &temp_path, "config.json")?;
                copy_session_file(&self.session_path, &temp_path, "delta.json")?;
            }
            let mut session = Session::load(&temp_path)?;
            session.execute_transaction(txn)
        })();

        let cleanup_result = std::fs::remove_dir_all(&temp_path);
        match (result, cleanup_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(e)) => Err(anyhow::anyhow!(
                "simulation completed but failed to remove temp session {}: {}",
                temp_path.display(),
                e
            )),
            (Err(e), _) => Err(e),
        }
    }

    /// Executes a transaction through the instrumented (tracing) VM path and
    /// returns the Foundry-style call trace.
    ///
    /// - `commit == false` (default): runs on a throwaway clone of the session,
    ///   like `simulate_transaction` — never mutates state.
    /// - `commit == true`: runs on the live session and commits the write set in
    ///   a single pass (same state effect as `submit`), so callers get the trace
    ///   tree AND the committed result without re-executing.
    pub fn execute_transaction_traced(
        &self,
        txn: SignedTransaction,
        commit: bool,
    ) -> Result<TraceResponse> {
        if commit {
            self.execute_transaction_traced_commit(txn)
        } else {
            self.execute_transaction_traced_dry(txn)
        }
    }

    fn execute_transaction_traced_dry(&self, txn: SignedTransaction) -> Result<TraceResponse> {
        let temp_path = make_temp_session_path("trace")?;
        std::fs::create_dir_all(&temp_path)?;

        let result = (|| {
            {
                let _session_guard = self.inner.lock().unwrap();
                copy_session_file(&self.session_path, &temp_path, "config.json")?;
                copy_session_file(&self.session_path, &temp_path, "delta.json")?;
            }
            let session = Session::load(&temp_path)?;
            let (_output, response) = trace_on(&session, &txn)?;
            Ok(response)
        })();

        let cleanup_result = std::fs::remove_dir_all(&temp_path);
        match (result, cleanup_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(e)) => Err(anyhow::anyhow!(
                "trace completed but failed to remove temp session {}: {}",
                temp_path.display(),
                e
            )),
            (Err(e), _) => Err(e),
        }
    }

    fn execute_transaction_traced_commit(&self, txn: SignedTransaction) -> Result<TraceResponse> {
        let committed_record = {
            let mut session = self.inner.lock().unwrap();
            let (txn_output, response) = trace_on(&session, &txn)?;
            // Commit the write set (gas + state) like `submit` does, regardless
            // of success — failed txns still consume gas and bump the sequence.
            session.commit_write_set(txn_output.write_set())?;
            response
        };

        // Mirror submit_transaction's bookkeeping so GET /transactions/by_hash
        // resolves the committed trace.
        self.increment_ops();
        let record = build_committed_record(&txn, &committed_record, self.get_ops_count());
        self.store_transaction(committed_record.txn_hash.clone(), record);
        Ok(committed_record)
    }

    pub fn store_transaction(&self, hash: String, result: serde_json::Value) {
        let mut store = self.tx_store.lock().unwrap();
        if store.len() >= 10_000 {
            if let Some(oldest) = store.keys().next().cloned() {
                store.remove(&oldest);
            }
        }
        store.insert(hash, result);
    }

    pub fn get_transaction(&self, hash: &str) -> Option<serde_json::Value> {
        let store = self.tx_store.lock().unwrap();
        store.get(hash).cloned()
    }

    pub fn get_module_bytes(&self, addr: AccountAddress, name: &str) -> Result<Option<Vec<u8>>> {
        let session = self.inner.lock().unwrap();
        let module_id = ModuleId::new(addr, Identifier::new(name)?);
        let state_key = StateKey::module_id(&module_id);
        match session.state_store().get_state_value_bytes(&state_key) {
            Ok(Some(bytes)) => Ok(Some(bytes.to_vec())),
            Ok(None) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("Failed to read module: {:?}", e)),
        }
    }

    pub fn get_chain_id(&self) -> u64 {
        self.chain_id
    }

    pub fn get_ops_count(&self) -> u64 {
        self.ops_count.lock().unwrap().clone()
    }

    pub fn increment_ops(&self) {
        let mut count = self.ops_count.lock().unwrap();
        *count += 1;
    }
}

pub fn create_session(options: SessionOptions) -> Result<SessionWrapper> {
    let session_path = match options.session_dir {
        Some(path) => path,
        None => make_temp_session_path("session")?,
    };

    if options.reset && session_path.exists() {
        std::fs::remove_dir_all(&session_path)?;
    }

    let session = if session_path.join("config.json").exists() {
        eprintln!("Loading session from {}...", session_path.display());
        Session::load(&session_path)?
    } else {
        if session_path.exists() && session_path.read_dir()?.next().is_some() {
            anyhow::bail!(
                "Session directory {} exists but is not a valid movelite session. Use --reset to replace it.",
                session_path.display()
            );
        }

        match options.fork_url {
            Some(url) => {
                eprintln!("Forking from {}...", redact_url_for_log(&url));
                let parsed_url = url::Url::parse(&url)?;
                let fork_version = options.fork_version.unwrap_or(0);
                Session::init_with_remote_state(&session_path, parsed_url, fork_version, None)?
            }
            None => {
                eprintln!("Initializing with clean genesis...");
                Session::init(&session_path)?
            }
        }
    };

    if let Some(chain_id) = options.chain_id {
        if chain_id == 0 {
            anyhow::bail!("--chain-id must be greater than 0");
        }
        session.state_store().set_chain_id(ChainId::new(chain_id))?;
    }

    let chain_id = session.state_store().get_chain_id()?.id() as u64;
    eprintln!("Session ready at {}.", session_path.display());
    Ok(SessionWrapper::new(session, session_path, chain_id))
}

/// Entry-frame seed extracted from the transaction payload (the entry function
/// produces no `charge_call`, so the tracer must be pre-seeded with it).
struct EntrySeed {
    module: Option<ModuleId>,
    function: Option<Identifier>,
    ty_args: Vec<TypeTag>,
    is_script: bool,
    /// User-provided arg count (excludes VM-injected leading `&signer`s).
    user_arg_count: usize,
}

fn extract_entry(txn: &SignedTransaction) -> EntrySeed {
    fn from_ef(ef: &EntryFunction) -> EntrySeed {
        EntrySeed {
            module: Some(ef.module().clone()),
            function: Some(ef.function().to_owned()),
            ty_args: ef.ty_args().to_vec(),
            is_script: false,
            user_arg_count: ef.args().len(),
        }
    }
    let script = EntrySeed {
        module: None,
        function: None,
        ty_args: vec![],
        is_script: true,
        // Script signer count isn't readily known here; don't strip.
        user_arg_count: usize::MAX,
    };
    match txn.payload() {
        TransactionPayload::EntryFunction(ef) => from_ef(ef),
        TransactionPayload::Script(_) => script,
        TransactionPayload::Payload(TransactionPayloadInner::V1 { executable, .. }) => {
            match executable {
                TransactionExecutable::EntryFunction(ef) => from_ef(ef),
                _ => script,
            }
        },
        _ => script,
    }
}

/// Replicates `Session::execute_transaction`'s VM wiring (session.rs:271-293)
/// but routes through `execute_user_transaction_with_modified_gas_meter` with a
/// `TracerMeter`, preserving production gas behavior. Returns the materialized
/// output (so the caller can commit its write set) and the built trace response.
fn trace_on(
    session: &Session,
    txn: &SignedTransaction,
) -> Result<(TransactionOutput, TraceResponse)> {
    let tx_hash = format!("0x{}", hex::encode(txn.committed_hash().to_vec()));
    let seed = extract_entry(txn);

    let state_store = session.state_store();
    let env = AptosEnvironment::new(state_store);
    let vm = AptosVM::new(&env, state_store);
    let log_context = AdapterLogSchema::new(state_store.id(), 0);
    let resolver = state_store.as_move_resolver();
    let code_storage = state_store.as_aptos_code_storage(&env);

    let aux = AuxiliaryInfo::new(
        PersistedAuxiliaryInfo::V1 {
            transaction_index: 0,
        },
        None,
    );

    let (vm_status, vm_output, meter) = vm
        .execute_user_transaction_with_modified_gas_meter(
            &resolver,
            &code_storage,
            txn,
            &log_context,
            |prod| {
                if seed.is_script {
                    TracerMeter::new_script(prod, seed.user_arg_count)
                } else {
                    TracerMeter::new_function(
                        prod,
                        seed.module.clone().expect("entry function has a module"),
                        seed.function.clone().expect("entry function has a name"),
                        seed.ty_args.clone(),
                        seed.user_arg_count,
                    )
                }
            },
            &aux,
        )
        .map_err(|status| anyhow::anyhow!("trace execution error: {:?}", status))?;

    let txn_output = vm_output
        .try_materialize_into_transaction_output(&resolver)
        .map_err(|status| anyhow::anyhow!("failed to materialize trace output: {:?}", status))?;
    let gas_used = txn_output.gas_used();

    let (mut root, abort_stack) = meter.finish();

    // Decode per-frame event payloads (TypeTag + BCS) to named JSON, using the
    // same annotator the REST API uses for resources/events. Falls back to hex.
    let annotator = aptos_resource_viewer::AptosValueAnnotator::new(state_store);
    let decode = |tag: &TypeTag, blob: &[u8]| -> serde_json::Value {
        let decoded: anyhow::Result<serde_json::Value> = annotator
            .view_value(tag, blob)
            .and_then(|av| Ok(aptos_api_types::MoveValue::try_from(av)?))
            .and_then(|mv| mv.json());
        decoded.unwrap_or_else(|_| serde_json::Value::String(format!("0x{}", hex::encode(blob))))
    };
    crate::trace::decode_events_in_tree(&mut root, &decode);

    let response = build_response(tx_hash, &vm_status, gas_used, root, abort_stack);
    Ok((txn_output, response))
}

/// Builds the committed-transaction record stored for GET /transactions/by_hash,
/// matching the shape `submit_transaction` produces.
fn build_committed_record(
    txn: &SignedTransaction,
    response: &TraceResponse,
    version: u64,
) -> serde_json::Value {
    serde_json::json!({
        "type": "user_transaction",
        "hash": response.txn_hash,
        "success": response.success,
        "vm_status": response.vm_status,
        "version": version.to_string(),
        "sender": format!("0x{}", hex::encode(txn.sender().to_vec())),
        "sequence_number": txn.sequence_number().to_string(),
        "max_gas_amount": txn.max_gas_amount().to_string(),
        "gas_unit_price": txn.gas_unit_price().to_string(),
        "expiration_timestamp_secs": txn.expiration_timestamp_secs().to_string(),
        "gas_used": response.gas_used.to_string(),
        "timestamp": "0"
    })
}

fn copy_session_file(from_dir: &PathBuf, to_dir: &PathBuf, file_name: &str) -> Result<()> {
    std::fs::copy(from_dir.join(file_name), to_dir.join(file_name))?;
    Ok(())
}

fn make_temp_session_path(kind: &str) -> Result<PathBuf> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(std::env::temp_dir().join(format!("movelite-{}-{}-{}", kind, std::process::id(), nanos)))
}

fn redact_url_for_log(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(mut parsed) => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.set_query(None);
            parsed.to_string()
        }
        Err(_) => "<invalid url>".to_string(),
    }
}
