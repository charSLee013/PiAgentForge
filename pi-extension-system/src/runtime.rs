//! WASM extension runtime powered by wasmtime.
//!
//! Loads a WASM module and communicates with it via JSON-over-linear-memory.
//!
//! # ABI
//!
//! The extension WASM module is expected to export:
//!
//! - `memory` – a linear memory used for all host–guest communication
//! - `alloc(size: i32) -> i32` – allocate `size` bytes and return a pointer
//! - `init()` – one-time initialisation (may call `register_tool`)
//! - `tool_handler(ptr: i32, len: i32) -> i32` – handle a tool invocation
//!
//! The host defines two importable functions in the `"pi"` module:
//!
//! - `log(ptr: i32, len: i32)` – log a string from guest memory
//! - `register_tool(ptr: i32, len: i32)` – register a JSON-serialised
//!   [`ToolDefinition`] so it appears in [`registered_tools`].
//!
//! The `tool_handler` result convention is:
//!   bytes 0..3  – `i32` length of the JSON result (little-endian)
//!   bytes 4..   – UTF-8 JSON result

use pi_ai_core::types::ToolDefinition;
use tracing::info;
use wasmtime::{AsContext, AsContextMut, Caller, Engine, Extern, Func, Instance, Linker, Memory, Module, Store};

use crate::sandbox::SandboxConfig;
use crate::types::{ExtensionError, Result};

// ---------------------------------------------------------------------------
// Extension context stored inside the wasmtime Store
// ---------------------------------------------------------------------------

/// Per-instance data that lives inside the wasmtime [`Store`].
struct ExtensionContext {
    /// Tools registered by the extension via the `register_tool` host function.
    registered_tools: Vec<ToolDefinition>,
    /// Events emitted by the extension via `emit_event`.
    event_queue: Vec<(i32, String)>,
}

// ---------------------------------------------------------------------------
// ExtensionRuntime
// ---------------------------------------------------------------------------

/// A running WASM extension instance.
///
/// Provides methods to initialise the extension, invoke its tools, and query
/// which tools it has registered.
pub struct ExtensionRuntime {
    #[allow(dead_code)]
    engine: Engine,
    store: Store<ExtensionContext>,
    /// Cached typed‑function handles for hot paths.
    init_func: Func,
    alloc_func: Func,
    tool_handler_func: Func,
    /// Optional handle_event export (extensions may not implement it).
    handle_event_func: Option<Func>,
    /// Memory reference, cached from the instance exports.
    memory: Memory,
}

impl ExtensionRuntime {
    /// Create a new runtime by loading the supplied WASM bytes.
    ///
    /// The module is validated, a sandbox with the given config is created, and
    /// the host‑side `"pi"` imports are wired up.
    pub fn new(wasm_bytes: &[u8], _config: SandboxConfig) -> Result<Self> {
        let engine = Engine::default();
        let module = Module::new(&engine, wasm_bytes)?;

        let mut linker = Linker::new(&engine);

        // ── Host function: pi.log ──────────────────────────────────────
        linker.func_wrap(
            "pi",
            "log",
            |mut caller: Caller<'_, ExtensionContext>, ptr: i32, len: i32| -> wasmtime::Result<()> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| match e {
                        Extern::Memory(m) => Some(m),
                        _ => None,
                    })
                    .ok_or_else(|| wasmtime::Error::msg("extension did not export 'memory'"))?;
                let msg = read_guest_string(&memory, &caller, ptr, len)?;
                info!("[extension] {}", msg);
                Ok(())
            },
        )?;

        // ── Host function: pi.register_tool ────────────────────────────
        linker.func_wrap(
            "pi",
            "register_tool",
            |mut caller: Caller<'_, ExtensionContext>, ptr: i32, len: i32| -> wasmtime::Result<()> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| match e {
                        Extern::Memory(m) => Some(m),
                        _ => None,
                    })
                    .ok_or_else(|| wasmtime::Error::msg("extension did not export 'memory'"))?;
                let json_str = read_guest_string(&memory, &caller, ptr, len)?;
                let tool: ToolDefinition =
                    serde_json::from_str(&json_str).map_err(|e| wasmtime::Error::msg(format!("{e}")))?;
                caller.data_mut().registered_tools.push(tool);
                Ok(())
            },
        )?;

        // ── Host function: pi.emit_event ───────────────────────────────
        // Extensions call this to notify the host about lifecycle events.
        // handle: event type identifier, data: JSON-serialized event payload.
        linker.func_wrap(
            "pi",
            "emit_event",
            |mut caller: Caller<'_, ExtensionContext>, handle: i32, ptr: i32, len: i32| -> wasmtime::Result<()> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| match e {
                        Extern::Memory(m) => Some(m),
                        _ => None,
                    })
                    .ok_or_else(|| wasmtime::Error::msg("extension did not export 'memory'"))?;
                let data = read_guest_string(&memory, &caller, ptr, len)?;
                caller.data_mut().event_queue.push((handle, data));
                Ok(())
            },
        )?;

        // ── Create store ──────────────────────────────────────────────
        let ctx = ExtensionContext {
            registered_tools: Vec::new(),
            event_queue: Vec::new(),
        };
        let mut store = Store::new(&engine, ctx);

        // ── Instantiate ───────────────────────────────────────────────
        let instance = linker.instantiate(&mut store, &module)?;

        // ── Extract exports ───────────────────────────────────────────
        let memory: Memory = take_export(&instance, &mut store, "memory")?;
        let init_func: Func = take_export(&instance, &mut store, "init")?;
        let alloc_func: Func = take_export(&instance, &mut store, "alloc")?;
        let tool_handler_func: Func = take_export(&instance, &mut store, "tool_handler")?;

        // handle_event is optional — old extensions may not export it.
        let handle_event_func = instance
            .get_export(&mut store, "handle_event")
            .and_then(|e| match e {
                Extern::Func(f) => Some(f),
                _ => None,
            });

        Ok(Self {
            engine,
            store,
            init_func,
            alloc_func,
            tool_handler_func,
            handle_event_func,
            memory,
        })
    }

    /// Call the extension's `init()` function.
    ///
    /// During init the extension typically registers its tools via
    /// `pi.register_tool`.
    pub fn call_init(&mut self) -> Result<()> {
        let typed = self
            .init_func
            .typed::<(), ()>(&self.store)
            .map_err(ExtensionError::Wasm)?;
        typed
            .call(&mut self.store, ())
            .map_err(|e| ExtensionError::Trap(format!("{e:#}")))?;
        Ok(())
    }

    /// Call the extension's `tool_handler` with a JSON‑encoded tool name and
    /// argument string.
    ///
    /// Returns the JSON result produced by the extension.
    pub fn call_tool(&mut self, _name: &str, args: &str) -> Result<String> {
        // 1. Allocate guest memory for args
        let args_bytes = args.as_bytes();
        let args_len = args_bytes.len() as i32;

        let alloc_typed = self
            .alloc_func
            .typed::<i32, i32>(&self.store)
            .map_err(ExtensionError::Wasm)?;
        let ptr = alloc_typed
            .call(&mut self.store, args_len)
            .map_err(|e| ExtensionError::Trap(format!("{e:#}")))?;

        // 2. Write args JSON to guest memory
        self.memory
            .write(&mut self.store, ptr as usize, args_bytes)
            .map_err(|e| ExtensionError::Wasm(e.into()))?;

        // 3. Call tool_handler
        let handler_typed = self
            .tool_handler_func
            .typed::<(i32, i32), i32>(&self.store)
            .map_err(ExtensionError::Wasm)?;
        let result_ptr = handler_typed
            .call(&mut self.store, (ptr, args_len))
            .map_err(|e| ExtensionError::Trap(format!("{e:#}")))?;

        // 4. Read result length (first 4 bytes)
        let mut len_buf = [0u8; 4];
        self.memory
            .read(&self.store, result_ptr as usize, &mut len_buf)
            .map_err(|e| ExtensionError::Wasm(e.into()))?;
        let result_len = i32::from_le_bytes(len_buf) as usize;

        // 5. Read result JSON
        let mut result_buf = vec![0u8; result_len];
        self.memory
            .read(&self.store, (result_ptr as usize) + 4, &mut result_buf)
            .map_err(|e| ExtensionError::Wasm(e.into()))?;

        // 6. Convert to String
        let result_str = String::from_utf8(result_buf)?;
        Ok(result_str)
    }

    /// Return the tools that the extension registered during `init()`.
    pub fn registered_tools(&self) -> &[ToolDefinition] {
        &self.store.data().registered_tools
    }

    /// Drain events that the extension emitted via `pi.emit_event`.
    pub fn drain_events(&mut self) -> Vec<(i32, String)> {
        std::mem::take(&mut self.store.data_mut().event_queue)
    }

    /// Deliver an event to the extension via its `handle_event` export.
    ///
    /// Writes `json_data` into guest memory and calls `handle_event(ptr, len)`.
    /// If the extension does not export `handle_event`, returns `None`.
    pub fn call_handle_event(&mut self, json_data: &str) -> Result<Option<String>> {
        let func = match &self.handle_event_func {
            Some(f) => *f,
            None => return Ok(None),
        };

        let bytes = json_data.as_bytes();
        let len = bytes.len() as i32;

        let alloc_typed = self
            .alloc_func
            .typed::<i32, i32>(&self.store)
            .map_err(ExtensionError::Wasm)?;
        let ptr = alloc_typed
            .call(&mut self.store, len)
            .map_err(|e| ExtensionError::Trap(format!("{e:#}")))?;

        self.memory
            .write(&mut self.store, ptr as usize, bytes)
            .map_err(|e| ExtensionError::Wasm(e.into()))?;

        let handler_typed = func
            .typed::<(i32, i32), i32>(&self.store)
            .map_err(ExtensionError::Wasm)?;
        let result_ptr = handler_typed
            .call(&mut self.store, (ptr, len))
            .map_err(|e| ExtensionError::Trap(format!("{e:#}")))?;

        if result_ptr == 0 {
            return Ok(None);
        }

        // Read result length (first 4 bytes)
        let mut len_buf = [0u8; 4];
        self.memory
            .read(&self.store, result_ptr as usize, &mut len_buf)
            .map_err(|e| ExtensionError::Wasm(e.into()))?;
        let result_len = i32::from_le_bytes(len_buf) as usize;

        // Read result JSON
        let mut result_buf = vec![0u8; result_len];
        self.memory
            .read(&self.store, (result_ptr as usize) + 4, &mut result_buf)
            .map_err(|e| ExtensionError::Wasm(e.into()))?;

        let result_str = String::from_utf8(result_buf)?;
        Ok(Some(result_str))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a named export from the instance, downcasting to the appropriate
/// wasmtime type (`Memory`, `Func`, etc.).
fn take_export<T>(instance: &Instance, store: &mut impl AsContextMut, name: &str) -> Result<T>
where
    T: ExportCast,
{
    instance
        .get_export(store, name)
        .and_then(|e| T::cast(e))
        .ok_or_else(|| ExtensionError::MissingExport(name.into()))
}

/// Helper trait for downcasting `Extern` to concrete wasmtime types.
trait ExportCast: Sized {
    fn cast(e: Extern) -> Option<Self>;
}

impl ExportCast for Memory {
    fn cast(e: Extern) -> Option<Self> {
        match e {
            Extern::Memory(m) => Some(m),
            _ => None,
        }
    }
}

impl ExportCast for Func {
    fn cast(e: Extern) -> Option<Self> {
        match e {
            Extern::Func(f) => Some(f),
            _ => None,
        }
    }
}

/// Read a UTF-8 string from guest linear memory.
fn read_guest_string(
    memory: &Memory,
    store: &impl AsContext,
    ptr: i32,
    len: i32,
) -> wasmtime::Result<String> {
    if len < 0 || ptr < 0 {
        return Err(wasmtime::Error::msg("negative pointer or length"));
    }
    let mut buf = vec![0u8; len as usize];
    memory
        .read(store, ptr as usize, &mut buf)
        .map_err(|e| wasmtime::Error::msg(format!("failed to read guest memory: {e}")))?;
    String::from_utf8(buf)
        .map_err(|e| wasmtime::Error::msg(format!("guest memory is not valid UTF-8: {e}")))
}
