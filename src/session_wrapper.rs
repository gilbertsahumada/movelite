use anyhow::Result;
use aptos_transaction_simulation::SimulationStateStore;
use aptos_transaction_simulation_session::Session;
use aptos_types::account_address::AccountAddress;
use aptos_types::chain_id::ChainId;
use aptos_types::state_store::state_key::StateKey;
use aptos_types::state_store::TStateView;
use aptos_types::transaction::{SignedTransaction, TransactionOutput};
use aptos_types::vm_status::VMStatus;
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
                "Session directory {} exists but is not a valid mvlite session. Use --reset to replace it.",
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

fn copy_session_file(from_dir: &PathBuf, to_dir: &PathBuf, file_name: &str) -> Result<()> {
    std::fs::copy(from_dir.join(file_name), to_dir.join(file_name))?;
    Ok(())
}

fn make_temp_session_path(kind: &str) -> Result<PathBuf> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(std::env::temp_dir().join(format!("mvlite-{}-{}-{}", kind, std::process::id(), nanos)))
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
