// Foundry-style execution trace for movelite.
//
// `TracerMeter<G>` wraps the production gas meter (`ProdGasMeter`) and, by
// observing the standard `GasMeter` callbacks the interpreter already emits,
// builds a tree of call frames. Each frame records the decoded
// `module::function`, type arguments, call arguments, self gas (bytecode +
// native), storage ops, emitted events and return values.
//
// Modeled on `aptos-gas-profiling`'s `GasProfiler` (same wrapper/delegation
// pattern), but specialized to produce the JSON contract movehat consumes
// instead of a flamegraph.
//
// Capability split (see docs/TRACE_FEASIBILITY.md):
//   - names / type_args / self-gas / nested args / storage ops: from GasMeter callbacks (this file)
//   - return values of non-native Move frames: needs the move-vm patch -> `record_move_return`
//   - per-frame events: needs the move-vm/framework patch -> `record_event`
// The two `record_*` hooks are gated behind `cfg(feature = "trace_patches")` so
// this file compiles both against vanilla aptos-core (PR-A) and the patched
// tree (PR-B/PR-C).

use aptos_gas_algebra::{Fee, FeePerGasUnit, InternalGas, NumArgs, NumBytes, NumTypeNodes};
use aptos_gas_meter::AptosGasMeter;
use aptos_types::{
    contract_event::ContractEvent, state_store::state_key::StateKey, write_set::WriteOpSize,
};
use move_binary_format::errors::PartialVMResult;
use move_binary_format::file_format::CodeOffset;
use move_core_types::{
    account_address::AccountAddress,
    identifier::{IdentStr, Identifier},
    language_storage::{ModuleId, TypeTag},
};
use move_vm_types::{
    gas::{DependencyGasMeter, DependencyKind, GasMeter, NativeGasMeter, SimpleInstruction},
    views::{TypeView, ValueView, ValueVisitor},
};
use serde::Serialize;
use serde_json::Value as Json;

// ---------------------------------------------------------------------------
// JSON contract types (fixed — see task spec). Serialized directly as response.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct TracedArg {
    #[serde(rename = "type")]
    pub ty: String,
    pub value: Json,
}

#[derive(Debug, Clone, Serialize)]
pub struct TracedEvent {
    #[serde(rename = "type")]
    pub ty: String,
    pub data: Json,
    /// Raw (type tag, BCS payload) captured at emit time; decoded into `data`
    /// post-execution by `decode_events_in_tree` (needs a resolver). Not serialized.
    #[serde(skip)]
    pub raw: Option<(TypeTag, Vec<u8>)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageOp {
    pub op: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub address: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AbortStackEntry {
    pub module: Option<String>,
    pub function: Option<String>,
    pub offset: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AbortInfo {
    pub code: u64,
    pub sub_status: Option<u64>,
    pub module: Option<String>,
    pub stack: Vec<AbortStackEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceResponse {
    pub txn_hash: String,
    pub success: bool,
    pub gas_used: u64,
    pub vm_status: String,
    pub abort: Option<AbortInfo>,
    pub root: CallNode,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallNode {
    pub kind: String, // "function" | "native" | "script"
    pub module: Option<String>,
    pub function: Option<String>,
    pub type_args: Vec<String>,
    pub args: Vec<TracedArg>,
    #[serde(rename = "return")]
    pub ret: Vec<TracedArg>,
    pub gas: u64, // SELF gas (bytecode + native), excluding children
    pub events: Vec<TracedEvent>,
    pub storage: Vec<StorageOp>,
    pub children: Vec<CallNode>,
}

/// In-progress frame: a `CallNode` plus the running self-gas counter and the
/// `is_native` flag we only learn at `charge_native_function` time.
struct FrameBuilder {
    node: CallNode,
    self_gas: InternalGas,
    native_gas: InternalGas,
}

impl FrameBuilder {
    fn function(module_id: ModuleId, name: Identifier, ty_args: Vec<TypeTag>) -> Self {
        Self {
            node: CallNode {
                kind: "function".to_string(),
                module: Some(format_module_id(&module_id)),
                function: Some(name.to_string()),
                type_args: ty_args.iter().map(|t| t.to_canonical_string()).collect(),
                args: vec![],
                ret: vec![],
                gas: 0,
                events: vec![],
                storage: vec![],
                children: vec![],
            },
            self_gas: 0.into(),
            native_gas: 0.into(),
        }
    }

    fn script() -> Self {
        Self {
            node: CallNode {
                kind: "script".to_string(),
                module: None,
                function: None,
                type_args: vec![],
                args: vec![],
                ret: vec![],
                gas: 0,
                events: vec![],
                storage: vec![],
                children: vec![],
            },
            self_gas: 0.into(),
            native_gas: 0.into(),
        }
    }

    fn finish(mut self) -> CallNode {
        self.node.gas = u64::from(self.self_gas) + u64::from(self.native_gas);
        self.node
    }
}

fn format_module_id(id: &ModuleId) -> String {
    format!("{}::{}", id.address().to_hex_literal(), id.name())
}

// ---------------------------------------------------------------------------
// The tracer gas meter.
// ---------------------------------------------------------------------------

pub struct TracerMeter<G> {
    base: G,
    /// Stack of in-progress frames. `frames[0]` is the (pre-seeded) entry frame
    /// that becomes the tree root; deeper entries are active nested calls.
    frames: Vec<FrameBuilder>,
    /// Number of user-provided entry arguments (from the txn payload, excluding
    /// the leading `&signer` params the VM injects). Used to strip signers from
    /// the recorded root args.
    user_arg_count: usize,
}

impl<G> TracerMeter<G> {
    pub fn new_function(
        base: G,
        module_id: ModuleId,
        func_name: Identifier,
        ty_args: Vec<TypeTag>,
        user_arg_count: usize,
    ) -> Self {
        Self {
            base,
            frames: vec![FrameBuilder::function(module_id, func_name, ty_args)],
            user_arg_count,
        }
    }

    pub fn new_script(base: G, user_arg_count: usize) -> Self {
        Self {
            base,
            frames: vec![FrameBuilder::script()],
            user_arg_count,
        }
    }

}

impl<G> TracerMeter<G>
where
    G: AptosGasMeter,
{
    fn active(&mut self) -> &mut FrameBuilder {
        self.frames.last_mut().expect("frame stack never empty")
    }

    /// Index of the deepest frame that is a sensible event sink: skips the
    /// `0x1::event` stdlib frames so events attach to the user function that
    /// called `event::emit` (matching Foundry semantics).
    fn event_sink_index(&self) -> usize {
        for (i, f) in self.frames.iter().enumerate().rev() {
            if f.node.module.as_deref() != Some("0x1::event") {
                return i;
            }
        }
        0
    }

    /// Delegate a charge to the base meter and return the gas it consumed.
    fn delegate_charge<F, R>(&mut self, charge: F) -> (InternalGas, R)
    where
        F: FnOnce(&mut G) -> R,
    {
        let old = self.base.balance_internal();
        let res = charge(&mut self.base);
        let new = self.base.balance_internal();
        let cost = old.checked_sub(new).unwrap_or_else(|| 0.into());
        (cost, res)
    }

    /// Delegate a charge and attribute its cost to the active frame's self gas.
    fn charge_to_active<F, R>(&mut self, charge: F) -> R
    where
        F: FnOnce(&mut G) -> R,
    {
        let (cost, res) = self.delegate_charge(charge);
        self.active().self_gas += cost;
        res
    }

    /// Pop the active (nested) frame and attach it to its parent's children.
    /// Never pops the root.
    fn pop_into_parent(&mut self) {
        if self.frames.len() > 1 {
            let done = self.frames.pop().unwrap().finish();
            self.active().node.children.push(done);
        }
    }
}

// Macro: delegate + attribute cost to the active frame, for the many plain
// bytecode charges that need no extra data captured.
macro_rules! charge_active {
    ($(
        fn $fn:ident $(<$($lt:lifetime),*>)? (&mut self $(, $arg:ident : $ty:ty)* $(,)?) -> PartialVMResult<()>;
    )*) => {
        $(fn $fn $(<$($lt),*>)? (&mut self, $($arg: $ty),*) -> PartialVMResult<()> {
            self.charge_to_active(|base| base.$fn($($arg),*))
        })*
    };
}

// Macro: pure delegation, no recording.
macro_rules! delegate_mut {
    ($(
        fn $fn:ident $(<$($lt:lifetime),*>)? (&mut self $(, $arg:ident : $ty:ty)* $(,)?) -> $ret:ty;
    )*) => {
        $(fn $fn $(<$($lt),*>)? (&mut self, $($arg: $ty),*) -> $ret {
            self.base.$fn($($arg),*)
        })*
    };
}

impl<G> DependencyGasMeter for TracerMeter<G>
where
    G: AptosGasMeter,
{
    fn charge_dependency(
        &mut self,
        kind: DependencyKind,
        addr: &AccountAddress,
        name: &IdentStr,
        size: NumBytes,
    ) -> PartialVMResult<()> {
        self.base.charge_dependency(kind, addr, name, size)
    }
}

impl<G> NativeGasMeter for TracerMeter<G>
where
    G: AptosGasMeter,
{
    fn legacy_gas_budget_in_native_context(&self) -> InternalGas {
        self.base.legacy_gas_budget_in_native_context()
    }

    fn use_heap_memory_in_native_context(&mut self, amount: u64) -> PartialVMResult<()> {
        self.base.use_heap_memory_in_native_context(amount)
    }

    fn charge_native_execution(&mut self, amount: InternalGas) -> PartialVMResult<()> {
        self.active().native_gas += amount;
        self.base.charge_native_execution(amount)
    }

    // Patch hook (PR-C): the framework event native calls this after building a
    // ContractEvent, so we can attach it to the active call frame.
    #[cfg(feature = "trace_patches")]
    fn record_event(&mut self, ty_tag: &TypeTag, data: &[u8]) {
        let idx = self.event_sink_index();
        self.frames[idx].node.events.push(TracedEvent {
            ty: ty_tag.to_canonical_string(),
            data: Json::Null,
            raw: Some((ty_tag.clone(), data.to_vec())),
        });
    }
}

impl<G> GasMeter for TracerMeter<G>
where
    G: AptosGasMeter,
{
    fn balance_internal(&self) -> InternalGas {
        self.base.balance_internal()
    }

    // Pure delegation: memory tracking / frame-drop callbacks that don't charge
    // execution gas we attribute per frame.
    delegate_mut! {
        fn charge_ld_const_after_deserialization(&mut self, val: impl ValueView) -> PartialVMResult<()>;
        fn charge_native_function_before_execution(
            &mut self,
            ty_args: impl ExactSizeIterator<Item = impl TypeView> + Clone,
            args: impl ExactSizeIterator<Item = impl ValueView> + Clone,
        ) -> PartialVMResult<()>;
        fn charge_drop_frame(
            &mut self,
            locals: impl Iterator<Item = impl ValueView> + Clone,
        ) -> PartialVMResult<()>;
    }

    charge_active! {
        fn charge_simple_instr(&mut self, instr: SimpleInstruction) -> PartialVMResult<()>;
        fn charge_br_true(&mut self, target_offset: Option<CodeOffset>) -> PartialVMResult<()>;
        fn charge_br_false(&mut self, target_offset: Option<CodeOffset>) -> PartialVMResult<()>;
        fn charge_branch(&mut self, target_offset: CodeOffset) -> PartialVMResult<()>;
        fn charge_pop(&mut self, popped_val: impl ValueView) -> PartialVMResult<()>;
        fn charge_ld_const(&mut self, size: NumBytes) -> PartialVMResult<()>;
        fn charge_copy_loc(&mut self, val: impl ValueView) -> PartialVMResult<()>;
        fn charge_move_loc(&mut self, val: impl ValueView) -> PartialVMResult<()>;
        fn charge_store_loc(&mut self, val: impl ValueView) -> PartialVMResult<()>;
        fn charge_pack(
            &mut self,
            is_generic: bool,
            args: impl ExactSizeIterator<Item = impl ValueView> + Clone,
        ) -> PartialVMResult<()>;
        fn charge_unpack(
            &mut self,
            is_generic: bool,
            args: impl ExactSizeIterator<Item = impl ValueView> + Clone,
        ) -> PartialVMResult<()>;
        fn charge_pack_closure(
            &mut self,
            is_generic: bool,
            args: impl ExactSizeIterator<Item = impl ValueView> + Clone,
        ) -> PartialVMResult<()>;
        fn charge_read_ref(&mut self, val: impl ValueView) -> PartialVMResult<()>;
        fn charge_write_ref(
            &mut self,
            new_val: impl ValueView,
            old_val: impl ValueView,
        ) -> PartialVMResult<()>;
        fn charge_eq(&mut self, lhs: impl ValueView, rhs: impl ValueView) -> PartialVMResult<()>;
        fn charge_neq(&mut self, lhs: impl ValueView, rhs: impl ValueView) -> PartialVMResult<()>;
        fn charge_exists(
            &mut self,
            is_generic: bool,
            ty: impl TypeView,
            exists: bool,
        ) -> PartialVMResult<()>;
        fn charge_vec_pack<'a>(
            &mut self,
            args: impl ExactSizeIterator<Item = impl ValueView> + Clone,
        ) -> PartialVMResult<()>;
        fn charge_vec_len(&mut self) -> PartialVMResult<()>;
        fn charge_vec_borrow(&mut self, is_mut: bool) -> PartialVMResult<()>;
        fn charge_vec_push_back(&mut self, val: impl ValueView) -> PartialVMResult<()>;
        fn charge_vec_pop_back(&mut self, val: Option<impl ValueView>) -> PartialVMResult<()>;
        fn charge_vec_unpack(
            &mut self,
            expect_num_elements: NumArgs,
            elems: impl ExactSizeIterator<Item = impl ValueView> + Clone,
        ) -> PartialVMResult<()>;
        fn charge_vec_swap(&mut self) -> PartialVMResult<()>;
        fn charge_create_ty(&mut self, num_nodes: NumTypeNodes) -> PartialVMResult<()>;
    }

    fn charge_call(
        &mut self,
        module_id: &ModuleId,
        func_name: &str,
        args: impl ExactSizeIterator<Item = impl ValueView> + Clone,
        num_locals: NumArgs,
    ) -> PartialVMResult<()> {
        let decoded: Vec<TracedArg> = args.clone().map(decode_value).collect();
        // CALL bytecode cost belongs to the caller (still the active frame).
        let res = self.charge_to_active(|base| base.charge_call(module_id, func_name, args, num_locals));
        let mut frame = FrameBuilder::function(
            module_id.clone(),
            Identifier::new(func_name).unwrap_or_else(|_| Identifier::new("unknown").unwrap()),
            vec![],
        );
        frame.node.args = decoded;
        self.frames.push(frame);
        res
    }

    fn charge_call_generic(
        &mut self,
        module_id: &ModuleId,
        func_name: &str,
        ty_args: impl ExactSizeIterator<Item = impl TypeView> + Clone,
        args: impl ExactSizeIterator<Item = impl ValueView> + Clone,
        num_locals: NumArgs,
    ) -> PartialVMResult<()> {
        let ty_tags: Vec<TypeTag> = ty_args.clone().map(|t| t.to_type_tag()).collect();
        let decoded: Vec<TracedArg> = args.clone().map(decode_value).collect();
        let res = self.charge_to_active(|base| {
            base.charge_call_generic(module_id, func_name, ty_args, args, num_locals)
        });
        let mut frame = FrameBuilder::function(
            module_id.clone(),
            Identifier::new(func_name).unwrap_or_else(|_| Identifier::new("unknown").unwrap()),
            ty_tags,
        );
        frame.node.args = decoded;
        self.frames.push(frame);
        res
    }

    fn charge_native_function(
        &mut self,
        amount: InternalGas,
        ret_vals: Option<impl ExactSizeIterator<Item = impl ValueView> + Clone>,
    ) -> PartialVMResult<()> {
        // `charge_call` blindly pushed a frame for this function; now we know it
        // is native. Capture its return values, mark it native, and fold it in.
        let returns: Vec<TracedArg> = ret_vals
            .clone()
            .map(|vals| vals.map(decode_value).collect())
            .unwrap_or_default();
        let res = self.charge_to_active(|base| base.charge_native_function(amount, ret_vals));
        // The native frame is the active one (unless it was the root, which
        // shouldn't happen for a native entry but guard anyway).
        if self.frames.len() > 1 {
            let frame = self.active();
            frame.node.kind = "native".to_string();
            frame.node.ret = returns;
            self.pop_into_parent();
        }
        res
    }

    fn charge_borrow_global(
        &mut self,
        is_mut: bool,
        is_generic: bool,
        ty: impl TypeView,
        is_success: bool,
    ) -> PartialVMResult<()> {
        let ty_tag = ty.to_type_tag();
        let res = self
            .charge_to_active(|base| base.charge_borrow_global(is_mut, is_generic, ty, is_success));
        if is_success {
            let op = if is_mut { "borrow_global_mut" } else { "borrow_global" };
            self.active().node.storage.push(StorageOp {
                op: op.to_string(),
                ty: ty_tag.to_canonical_string(),
                address: None, // not provided by this callback
            });
        }
        res
    }

    fn charge_move_from(
        &mut self,
        is_generic: bool,
        ty: impl TypeView,
        val: Option<impl ValueView>,
    ) -> PartialVMResult<()> {
        let ty_tag = ty.to_type_tag();
        let res = self.charge_to_active(|base| base.charge_move_from(is_generic, ty, val));
        self.active().node.storage.push(StorageOp {
            op: "move_from".to_string(),
            ty: ty_tag.to_canonical_string(),
            address: None,
        });
        res
    }

    fn charge_move_to(
        &mut self,
        is_generic: bool,
        ty: impl TypeView,
        val: impl ValueView,
        is_success: bool,
    ) -> PartialVMResult<()> {
        let ty_tag = ty.to_type_tag();
        let res =
            self.charge_to_active(|base| base.charge_move_to(is_generic, ty, val, is_success));
        if is_success {
            self.active().node.storage.push(StorageOp {
                op: "move_to".to_string(),
                ty: ty_tag.to_canonical_string(),
                address: None,
            });
        }
        res
    }

    fn charge_load_resource(
        &mut self,
        addr: AccountAddress,
        ty: impl TypeView,
        val: Option<impl ValueView>,
        bytes_loaded: NumBytes,
    ) -> PartialVMResult<()> {
        let ty_tag = ty.to_type_tag();
        let res =
            self.charge_to_active(|base| base.charge_load_resource(addr, ty, val, bytes_loaded));
        self.active().node.storage.push(StorageOp {
            op: "load_resource".to_string(),
            ty: ty_tag.to_canonical_string(),
            address: Some(addr.to_hex_literal()),
        });
        res
    }

    // Patch hook (PR-B): the interpreter calls this in the `Return` arm with the
    // returning Move frame's values, before the frame is popped. This is also
    // what gives correct sibling structure — Move frames pop here, not on RET.
    #[cfg(feature = "trace_patches")]
    fn record_move_return(&mut self, ret_vals: impl ExactSizeIterator<Item = impl ValueView> + Clone) {
        let decoded: Vec<TracedArg> = ret_vals.map(decode_value).collect();
        let decoded = if decoded.is_empty() {
            vec![unit_arg()]
        } else {
            decoded
        };
        self.active().node.ret = decoded;
        // This Move frame is now complete; fold it into its parent.
        self.pop_into_parent();
    }

    // Patch hook (PR-B): fires at every interpreter entry (prologue, main,
    // epilogue). We record args only for the genuine entry frame, identified by
    // matching the executing function against the pre-seeded root.
    #[cfg(feature = "trace_patches")]
    fn record_entry_args(
        &mut self,
        module_id: Option<&ModuleId>,
        name: &str,
        args: impl ExactSizeIterator<Item = impl ValueView> + Clone,
    ) {
        if self.frames.len() != 1 || !self.frames[0].node.args.is_empty() {
            return;
        }
        let matches = match (&self.frames[0].node.function, module_id) {
            // Entry function: module + name must match the pre-seeded root.
            (Some(want_fn), Some(mid)) => {
                Some(format_module_id(mid)) == self.frames[0].node.module && name == want_fn
            },
            // Script root: the entry has no module.
            (None, None) => true,
            _ => false,
        };
        if matches {
            // Strip leading `&signer` params (VM-injected, not in the txn args).
            let skip = args.len().saturating_sub(self.user_arg_count);
            self.frames[0].node.args = args.skip(skip).map(decode_value).collect();
        }
    }
}

impl<G> AptosGasMeter for TracerMeter<G>
where
    G: AptosGasMeter,
{
    type Algebra = G::Algebra;

    fn algebra(&self) -> &Self::Algebra {
        self.base.algebra()
    }

    delegate_mut! {
        fn algebra_mut(&mut self) -> &mut Self::Algebra;
        fn charge_storage_fee(
            &mut self,
            amount: Fee,
            gas_unit_price: FeePerGasUnit,
        ) -> PartialVMResult<()>;
        fn charge_intrinsic_gas_for_transaction(&mut self, txn_size: NumBytes) -> move_binary_format::errors::VMResult<()>;
        fn charge_keyless(&mut self) -> move_binary_format::errors::VMResult<()>;
        fn charge_io_gas_for_transaction(&mut self, txn_size: NumBytes) -> move_binary_format::errors::VMResult<()>;
        fn charge_io_gas_for_event(&mut self, event: &ContractEvent) -> move_binary_format::errors::VMResult<()>;
        fn charge_io_gas_for_write(&mut self, key: &StateKey, op: &WriteOpSize) -> move_binary_format::errors::VMResult<()>;
    }
}

impl<G> TracerMeter<G>
where
    G: AptosGasMeter,
{
    /// Collapse any frames still on the stack into the root and return it,
    /// along with the active call chain (innermost first). On success only the
    /// root remains; on abort the still-open frames ARE the abort call stack
    /// (already-returned siblings have been popped, so they are excluded).
    pub fn finish(mut self) -> (CallNode, Vec<AbortStackEntry>) {
        let active_chain: Vec<AbortStackEntry> = self
            .frames
            .iter()
            .rev()
            .map(|f| AbortStackEntry {
                module: f.node.module.clone(),
                function: f.node.function.clone(),
                offset: None,
            })
            .collect();
        while self.frames.len() > 1 {
            let done = self.frames.pop().unwrap().finish();
            self.active().node.children.push(done);
        }
        (self.frames.pop().unwrap().finish(), active_chain)
    }
}

// ---------------------------------------------------------------------------
// Response assembly.
// ---------------------------------------------------------------------------

use aptos_types::vm_status::{AbortLocation, VMStatus};

pub fn build_response(
    txn_hash: String,
    status: &VMStatus,
    gas_used: u64,
    root: CallNode,
    abort_stack: Vec<AbortStackEntry>,
) -> TraceResponse {
    let success = matches!(status, VMStatus::Executed);
    let vm_status = if success {
        "Executed successfully".to_string()
    } else {
        format!("{:?}", status)
    };
    let abort = if success {
        None
    } else {
        Some(build_abort(status, abort_stack))
    };
    TraceResponse {
        txn_hash,
        success,
        gas_used,
        vm_status,
        abort,
        root,
    }
}

fn abort_location_module(loc: &AbortLocation) -> Option<String> {
    match loc {
        AbortLocation::Module(id) => Some(format_module_id(id)),
        AbortLocation::Script => None,
    }
}

/// Builds the abort info from the active call chain captured at abort time
/// (innermost first) plus the abort code/module from the VM status.
fn build_abort(status: &VMStatus, mut stack: Vec<AbortStackEntry>) -> AbortInfo {
    let innermost_offset = match status {
        VMStatus::ExecutionFailure { code_offset, .. } => Some(*code_offset as u64),
        _ => None,
    };
    if let (Some(first), Some(off)) = (stack.first_mut(), innermost_offset) {
        first.offset = Some(off);
    }

    let (code, sub_status, module) = match status {
        VMStatus::MoveAbort(loc, code) => (*code, None, abort_location_module(loc)),
        VMStatus::ExecutionFailure {
            status_code,
            sub_status,
            location,
            ..
        } => (
            *status_code as u64,
            *sub_status,
            abort_location_module(location),
        ),
        VMStatus::Error {
            status_code,
            sub_status,
            ..
        } => (*status_code as u64, *sub_status, None),
        VMStatus::Executed => (0, None, None),
    };
    // Fall back to the innermost frame's module if the status lacks a location.
    let module = module.or_else(|| stack.first().and_then(|n| n.module.clone()));

    AbortInfo {
        code,
        sub_status,
        module,
        stack,
    }
}

// ---------------------------------------------------------------------------
// Value decoding (ValueView -> contract JSON, structural / by-index).
// ---------------------------------------------------------------------------

pub fn unit_arg() -> TracedArg {
    TracedArg {
        ty: "()".to_string(),
        value: Json::Null,
    }
}

fn decode_value(v: impl ValueView) -> TracedArg {
    let mut b = JsonBuilder::default();
    if v.visit(&mut b).is_err() {
        return TracedArg {
            ty: "unknown".to_string(),
            value: Json::Null,
        };
    }
    TracedArg {
        ty: b.first_type.unwrap_or_else(|| "unknown".to_string()),
        value: b.root.unwrap_or(Json::Null),
    }
}

enum ContainerKind {
    Struct,
    Vector,
}

struct Container {
    kind: ContainerKind,
    items: Vec<Json>,
    remaining: usize,
}

impl Container {
    fn into_json(self) -> Json {
        match self.kind {
            ContainerKind::Vector => Json::Array(self.items),
            ContainerKind::Struct => {
                let mut map = serde_json::Map::new();
                for (i, item) in self.items.into_iter().enumerate() {
                    map.insert(i.to_string(), item);
                }
                Json::Object(map)
            },
        }
    }
}

#[derive(Default)]
struct JsonBuilder {
    stack: Vec<Container>,
    root: Option<Json>,
    first_type: Option<String>,
}

impl JsonBuilder {
    fn set_type(&mut self, t: &str) {
        if self.first_type.is_none() {
            self.first_type = Some(t.to_string());
        }
    }

    /// Add a finished value, folding completed containers into their parents.
    fn add(&mut self, value: Json) {
        let mut cur = value;
        loop {
            match self.stack.last_mut() {
                None => {
                    self.root = Some(cur);
                    return;
                },
                Some(frame) => {
                    frame.items.push(cur);
                    frame.remaining = frame.remaining.saturating_sub(1);
                    if frame.remaining == 0 {
                        cur = self.stack.pop().unwrap().into_json();
                        // loop: fold into parent
                    } else {
                        return;
                    }
                },
            }
        }
    }

    fn open(&mut self, kind: ContainerKind, len: usize) {
        if len == 0 {
            let empty = Container {
                kind,
                items: vec![],
                remaining: 0,
            };
            self.add(empty.into_json());
        } else {
            self.stack.push(Container {
                kind,
                items: Vec::with_capacity(len),
                remaining: len,
            });
        }
    }
}

macro_rules! visit_int {
    ($name:ident, $t:ty, $label:literal) => {
        fn $name(&mut self, _depth: u64, val: $t) -> PartialVMResult<()> {
            self.set_type($label);
            self.add(Json::String(val.to_string()));
            Ok(())
        }
    };
}

impl ValueVisitor for JsonBuilder {
    visit_int!(visit_u8, u8, "u8");
    visit_int!(visit_u16, u16, "u16");
    visit_int!(visit_u32, u32, "u32");
    visit_int!(visit_u64, u64, "u64");
    visit_int!(visit_u128, u128, "u128");
    visit_int!(visit_i8, i8, "i8");
    visit_int!(visit_i16, i16, "i16");
    visit_int!(visit_i32, i32, "i32");
    visit_int!(visit_i64, i64, "i64");
    visit_int!(visit_i128, i128, "i128");

    fn visit_u256(
        &mut self,
        _depth: u64,
        val: &move_core_types::int256::U256,
    ) -> PartialVMResult<()> {
        self.set_type("u256");
        self.add(Json::String(val.to_string()));
        Ok(())
    }

    fn visit_i256(
        &mut self,
        _depth: u64,
        val: &move_core_types::int256::I256,
    ) -> PartialVMResult<()> {
        self.set_type("i256");
        self.add(Json::String(val.to_string()));
        Ok(())
    }

    fn visit_bool(&mut self, _depth: u64, val: bool) -> PartialVMResult<()> {
        self.set_type("bool");
        self.add(Json::Bool(val));
        Ok(())
    }

    fn visit_address(&mut self, _depth: u64, val: &AccountAddress) -> PartialVMResult<()> {
        self.set_type("address");
        self.add(Json::String(val.to_hex_literal()));
        Ok(())
    }

    fn visit_delayed(
        &mut self,
        _depth: u64,
        _id: move_vm_types::delayed_values::delayed_field_id::DelayedFieldID,
    ) -> PartialVMResult<()> {
        self.set_type("delayed");
        self.add(Json::String("<delayed>".to_string()));
        Ok(())
    }

    fn visit_struct(&mut self, _depth: u64, len: usize) -> PartialVMResult<bool> {
        self.set_type("struct");
        self.open(ContainerKind::Struct, len);
        Ok(true)
    }

    fn visit_closure(&mut self, _depth: u64, len: usize) -> PartialVMResult<bool> {
        self.set_type("closure");
        self.open(ContainerKind::Struct, len);
        Ok(true)
    }

    fn visit_vec(&mut self, _depth: u64, len: usize) -> PartialVMResult<bool> {
        self.set_type("vector");
        self.open(ContainerKind::Vector, len);
        Ok(true)
    }

    fn visit_ref(&mut self, _depth: u64, _is_global: bool) -> PartialVMResult<bool> {
        // Transparent: the referent becomes the value for this slot.
        Ok(true)
    }

    // Render byte vectors as a hex string rather than an array of numbers.
    fn visit_vec_u8(&mut self, _depth: u64, vals: &[u8]) -> PartialVMResult<()> {
        self.set_type("vector<u8>");
        self.add(Json::String(format!("0x{}", hex::encode(vals))));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Event data decoding (PR-C). Walks the finished tree and turns each event's
// raw (TypeTag, BCS) into named JSON via a caller-supplied decoder (which holds
// the resolver/annotator — see session_wrapper::run_traced).
// ---------------------------------------------------------------------------

pub fn decode_events_in_tree(node: &mut CallNode, decode: &impl Fn(&TypeTag, &[u8]) -> Json) {
    for ev in &mut node.events {
        if let Some((tag, blob)) = ev.raw.take() {
            ev.data = decode(&tag, &blob);
        }
    }
    for child in &mut node.children {
        decode_events_in_tree(child, decode);
    }
}
