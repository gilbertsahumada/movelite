use anyhow::Result;
use aptos_transaction_simulation_session::{BlockTimestamp, Session};
use aptos_types::account_address::AccountAddress;
use aptos_types::state_store::state_key::StateKey;
use aptos_types::state_store::TStateView;
use aptos_types::transaction::{SignedTransaction, TransactionOutput};
use aptos_types::vm_status::VMStatus;
use move_core_types::identifier::Identifier;
use move_core_types::language_storage::{ModuleId, StructTag, TypeTag};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct SessionWrapper {
    inner: Mutex<Session>,
    ops_count: Mutex<u64>,
    tx_store: Mutex<HashMap<String, serde_json::Value>>,
}

impl SessionWrapper {
    pub fn new(session: Session) -> Self {
        Self {
            inner: Mutex::new(session),
            ops_count: Mutex::new(0),
            tx_store: Mutex::new(HashMap::new()),
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
        session.execute_view_function(module_id, function_name, ty_args, args, false, false)
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
        let result = session.execute_transaction(txn, false, false)?;
        session.new_block(BlockTimestamp::Default)?;
        Ok(result)
    }

    pub fn simulate_transaction(
        &self,
        txn: SignedTransaction,
    ) -> Result<(VMStatus, TransactionOutput)> {
        // Known limitation: the Session API has no snapshot/rollback,
        // so simulation applies the write set to the state store.
        // Gas estimation is accurate but state side-effects persist.
        // For a dev tool this is acceptable. A proper fix requires
        // cloning the state store or adding snapshot support upstream.
        let mut session = self.inner.lock().unwrap();
        let result = session.execute_transaction(txn, false, false)?;
        // Do NOT advance block — this distinguishes simulate from submit.
        Ok(result)
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

    pub fn has_any_state(&self, addr: AccountAddress) -> bool {
        let session = self.inner.lock().unwrap();
        let fungible_tag = StructTag {
            address: AccountAddress::ONE,
            module: Identifier::new("fungible_asset").unwrap(),
            name: Identifier::new("FungibleStore").unwrap(),
            type_args: vec![],
        };
        let key = StateKey::resource(&addr, &fungible_tag).unwrap();
        matches!(session.state_store().get_state_value_bytes(&key), Ok(Some(_)))
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
        4
    }

    pub fn get_ops_count(&self) -> u64 {
        self.ops_count.lock().unwrap().clone()
    }

    pub fn increment_ops(&self) {
        let mut count = self.ops_count.lock().unwrap();
        *count += 1;
    }
}

pub fn create_session(fork_url: Option<String>) -> Result<SessionWrapper> {
    let session_path = PathBuf::from(".mvlite-session");

    if session_path.exists() {
        std::fs::remove_dir_all(&session_path)?;
    }

    let mut session = match fork_url {
        Some(url) => {
            println!("Forking from {}...", url);
            let parsed_url = url::Url::parse(&url)?;
            Session::init_with_remote_state(&session_path, parsed_url, 0, None)?
        }
        None => {
            println!("Initializing with clean genesis...");
            Session::init(&session_path)?
        }
    };

    session.new_block(BlockTimestamp::Default)?;
    println!("Session ready.");
    Ok(SessionWrapper::new(session))
}
