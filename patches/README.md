# aptos-core patches (movelite trace hooks)

These patches add the move-vm / framework callbacks that movelite's tracing gas
meter (`src/trace.rs`, behind the `trace_patches` cargo feature) relies on to
produce Foundry-style execution traces via `POST /transactions/trace`.

They are applied automatically by `build.sh` after the pinned aptos-core
checkout (commit `e33e3c1b9e`). Every hook is a **default no-op** on the
`GasMeter` / `NativeGasMeter` traits, so for every other gas meter in aptos-core
(`ProdGasMeter`, `UnmeteredGasMeter`, `GasProfiler`, …) the patched code behaves
identically to upstream. Normal `submit`/`simulate` execution is unaffected.

## `0001-movelite-trace-hooks.patch`

Touches three files, covering both move-vm fork concerns:

| File | Change | Trace feature item |
|------|--------|--------------------|
| `third_party/move/move-vm/types/src/gas.rs` | Adds 3 default-no-op trait methods: `GasMeter::record_move_return`, `GasMeter::record_entry_args` (item 4) and `NativeGasMeter::record_event` (item 5). Imports `TypeTag`. | 4 + 5 |
| `third_party/move/move-vm/runtime/src/interpreter.rs` | In `execute_main`: calls `record_entry_args` at entry (root frame args, which don't flow through `charge_call`). In the `ExitCode::Return` arm: calls `record_move_return` with the returning Move frame's values before the frame is popped (also gives correct sibling tree structure). | 4 |
| `aptos-move/framework/src/natives/event.rs` | In both event-emission natives, calls `context.gas_meter().record_event(type_tag, blob)` after building the `ContractEvent`, so the tracer can attribute the event to the active call frame. | 5 |

### Item 4 — return values (`record_move_return` / `record_entry_args`)
Native function return values already reach the gas meter via
`charge_native_function(amount, ret_vals)`, so only **non-native Move** frames
need a hook. The interpreter peeks the top N operand-stack values (where N =
`function.return_tys().len()`) in the `Return` arm and forwards them, without
consuming them, before popping the frame.

### Item 5 — per-frame events (`record_event`)
Events are buffered at session level with no native per-frame attribution. The
hook routes each emitted event (type tag + BCS payload) through the gas meter,
which owns the live frame stack, giving exact attribution. The tracer attaches
the event to the nearest non-`0x1::event` ancestor frame (Foundry semantics).

## `0002-session-commit-write-set.patch`

Touches `aptos-move/aptos-transaction-simulation-session/src/session.rs`: adds
`Session::commit_write_set(&mut self, &WriteSet)`, which applies a write set
produced by an out-of-band execution to the live state store and persists it
(`config.ops++` + `save_to_file` + `save_delta`) — the same commit tail as
`execute_transaction`. This lets movelite run the VM with the tracing gas meter
**and** commit through the session in a single pass (powers
`POST /transactions/trace?commit=true`). Plain `pub` method, not feature-gated.

## Reapplying / reversing

`build.sh` applies every `patches/aptos-core/*.patch` idempotently after checkout.

```bash
# Apply all patches manually:
for p in patches/aptos-core/*.patch; do git -C .aptos-core apply "$p"; done

# Check whether a patch is already applied:
git -C .aptos-core apply --reverse --check patches/aptos-core/0001-movelite-trace-hooks.patch

# Revert to a pristine aptos-core:
for p in patches/aptos-core/*.patch; do git -C .aptos-core apply --reverse "$p"; done
```

## Regenerating after editing the hooks

```bash
git -C .aptos-core diff -- \
  third_party/move/move-vm/types/src/gas.rs \
  third_party/move/move-vm/runtime/src/interpreter.rs \
  aptos-move/framework/src/natives/event.rs \
  > patches/aptos-core/0001-movelite-trace-hooks.patch

git -C .aptos-core diff -- \
  aptos-move/aptos-transaction-simulation-session/src/session.rs \
  > patches/aptos-core/0002-session-commit-write-set.patch
```
