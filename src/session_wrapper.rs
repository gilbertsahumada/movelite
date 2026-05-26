use anyhow::Result;
use aptos_transaction_simulation_session::{BlockTimestamp, Session};
use aptos_types::account_address::AccountAddress;
use aptos_types::transaction::{SignedTransaction, TransactionOutput};
use aptos_types::vm_status::VMStatus;
use move_core_types::identifier::Identifier;
use move_core_types::language_storage::{ModuleId, StructTag, TypeTag};
use std::path::PathBuf;
use std::sync::Mutex;

pub struct SessionWrapper {
    inner: Mutex<Session>,
    ops_count: Mutex<u64>,
}

impl SessionWrapper {
    pub fn new(session: Session) -> Self {
        Self {
            inner: Mutex::new(session),
            ops_count: Mutex::new(0),
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
