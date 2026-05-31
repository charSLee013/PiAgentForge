//! Integration tests for the WASM extension system.
//!
//! These tests compile small WAT modules and exercise the full
//! ExtensionRuntime lifecycle (init, tool_call, ABI).

/// Minimal WAT module:
/// - exports `memory`, `alloc`, `init`, `tool_handler`
/// - `init` is a no-op
/// - `tool_handler` returns `{"result":"ok"}` regardless of input
const MINIMAL_EXTENSION_WAT: &str = r#"
(module
    (memory (export "memory") 1)
    (global $heap_ptr (mut i32) (i32.const 0))

    (func $alloc (export "alloc") (param $size i32) (result i32)
        (local $ptr i32)
        (local.set $ptr (global.get $heap_ptr))
        (global.set $heap_ptr
            (i32.add (global.get $heap_ptr) (local.get $size))
        )
        (local.get $ptr)
    )

    (func $init (export "init"))

    (func $tool_handler (export "tool_handler") (param $ptr i32) (param $len i32) (result i32)
        (local $result_ptr i32)

        ;; Allocate 19 bytes: 4 for length + 15 for JSON
        (local.set $result_ptr (call $alloc (i32.const 19)))

        ;; Write result length (15) at result_ptr[0..4]
        (i32.store (local.get $result_ptr) (i32.const 15))

        ;; Write '{"result":"ok"}' at result_ptr[4..19]
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 4))  (i32.const 123))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 5))  (i32.const 34))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 6))  (i32.const 114))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 7))  (i32.const 101))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 8))  (i32.const 115))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 9))  (i32.const 117))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 10)) (i32.const 108))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 11)) (i32.const 116))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 12)) (i32.const 34))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 13)) (i32.const 58))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 14)) (i32.const 34))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 15)) (i32.const 111))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 16)) (i32.const 107))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 17)) (i32.const 34))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 18)) (i32.const 125))

        (local.get $result_ptr)
    )
)
"#;

/// Extension that imports and calls `pi.register_tool` during init.
///
/// The JSON registered is (92 bytes):
/// {"name":"my_tool","description":"A test tool","parameters":{"type":"object","properties":{}}}
const EXTENSION_WITH_TOOL_WAT: &str = r#"
(module
    (import "pi" "register_tool" (func $register_tool (param i32 i32)))
    (memory (export "memory") 1)
    (global $heap_ptr (mut i32) (i32.const 0))

    (func $alloc (export "alloc") (param $size i32) (result i32)
        (local $ptr i32)
        (local.set $ptr (global.get $heap_ptr))
        (global.set $heap_ptr
            (i32.add (global.get $heap_ptr) (local.get $size))
        )
        (local.get $ptr)
    )

    (func $init (export "init")
        ;; Register tool: {"name":"my_tool","description":"A test tool","parameters":{"type":"object","properties":{}}}
        ;; JSON is 93 bytes
        (local $ptr i32)
        (local.set $ptr (call $alloc (i32.const 93)))

        ;; Write JSON bytes at $ptr — byte by byte
        ;; {"name":"my_tool","description":"A test tool","parameters":{"type":"object","properties":{}}}
        (i32.store8 (local.get $ptr)           (i32.const 123))  ;; 0  {
        (i32.store8 (i32.add (local.get $ptr) (i32.const 1))  (i32.const 34))   ;; 1  "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 2))  (i32.const 110))  ;; 2  n
        (i32.store8 (i32.add (local.get $ptr) (i32.const 3))  (i32.const 97))   ;; 3  a
        (i32.store8 (i32.add (local.get $ptr) (i32.const 4))  (i32.const 109))  ;; 4  m
        (i32.store8 (i32.add (local.get $ptr) (i32.const 5))  (i32.const 101))  ;; 5  e
        (i32.store8 (i32.add (local.get $ptr) (i32.const 6))  (i32.const 34))   ;; 6  "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 7))  (i32.const 58))   ;; 7  :
        (i32.store8 (i32.add (local.get $ptr) (i32.const 8))  (i32.const 34))   ;; 8  "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 9))  (i32.const 109))  ;; 9  m
        (i32.store8 (i32.add (local.get $ptr) (i32.const 10)) (i32.const 121))  ;; 10 y
        (i32.store8 (i32.add (local.get $ptr) (i32.const 11)) (i32.const 95))   ;; 11 _
        (i32.store8 (i32.add (local.get $ptr) (i32.const 12)) (i32.const 116))  ;; 12 t
        (i32.store8 (i32.add (local.get $ptr) (i32.const 13)) (i32.const 111))  ;; 13 o
        (i32.store8 (i32.add (local.get $ptr) (i32.const 14)) (i32.const 111))  ;; 14 o
        (i32.store8 (i32.add (local.get $ptr) (i32.const 15)) (i32.const 108))  ;; 15 l
        (i32.store8 (i32.add (local.get $ptr) (i32.const 16)) (i32.const 34))   ;; 16 "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 17)) (i32.const 44))   ;; 17 ,
        (i32.store8 (i32.add (local.get $ptr) (i32.const 18)) (i32.const 34))   ;; 18 "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 19)) (i32.const 100))  ;; 19 d
        (i32.store8 (i32.add (local.get $ptr) (i32.const 20)) (i32.const 101))  ;; 20 e
        (i32.store8 (i32.add (local.get $ptr) (i32.const 21)) (i32.const 115))  ;; 21 s
        (i32.store8 (i32.add (local.get $ptr) (i32.const 22)) (i32.const 99))   ;; 22 c
        (i32.store8 (i32.add (local.get $ptr) (i32.const 23)) (i32.const 114))  ;; 23 r
        (i32.store8 (i32.add (local.get $ptr) (i32.const 24)) (i32.const 105))  ;; 24 i
        (i32.store8 (i32.add (local.get $ptr) (i32.const 25)) (i32.const 112))  ;; 25 p
        (i32.store8 (i32.add (local.get $ptr) (i32.const 26)) (i32.const 116))  ;; 26 t
        (i32.store8 (i32.add (local.get $ptr) (i32.const 27)) (i32.const 105))  ;; 27 i
        (i32.store8 (i32.add (local.get $ptr) (i32.const 28)) (i32.const 111))  ;; 28 o
        (i32.store8 (i32.add (local.get $ptr) (i32.const 29)) (i32.const 110))  ;; 29 n
        (i32.store8 (i32.add (local.get $ptr) (i32.const 30)) (i32.const 34))   ;; 30 "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 31)) (i32.const 58))   ;; 31 :
        (i32.store8 (i32.add (local.get $ptr) (i32.const 32)) (i32.const 34))   ;; 32 "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 33)) (i32.const 65))   ;; 33 A
        (i32.store8 (i32.add (local.get $ptr) (i32.const 34)) (i32.const 32))   ;; 34 (space)
        (i32.store8 (i32.add (local.get $ptr) (i32.const 35)) (i32.const 116))  ;; 35 t
        (i32.store8 (i32.add (local.get $ptr) (i32.const 36)) (i32.const 101))  ;; 36 e
        (i32.store8 (i32.add (local.get $ptr) (i32.const 37)) (i32.const 115))  ;; 37 s
        (i32.store8 (i32.add (local.get $ptr) (i32.const 38)) (i32.const 116))  ;; 38 t
        (i32.store8 (i32.add (local.get $ptr) (i32.const 39)) (i32.const 32))   ;; 39 (space)
        (i32.store8 (i32.add (local.get $ptr) (i32.const 40)) (i32.const 116))  ;; 40 t
        (i32.store8 (i32.add (local.get $ptr) (i32.const 41)) (i32.const 111))  ;; 41 o
        (i32.store8 (i32.add (local.get $ptr) (i32.const 42)) (i32.const 111))  ;; 42 o
        (i32.store8 (i32.add (local.get $ptr) (i32.const 43)) (i32.const 108))  ;; 43 l
        (i32.store8 (i32.add (local.get $ptr) (i32.const 44)) (i32.const 34))   ;; 44 "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 45)) (i32.const 44))   ;; 45 ,
        (i32.store8 (i32.add (local.get $ptr) (i32.const 46)) (i32.const 34))   ;; 46 "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 47)) (i32.const 112))  ;; 47 p
        (i32.store8 (i32.add (local.get $ptr) (i32.const 48)) (i32.const 97))   ;; 48 a
        (i32.store8 (i32.add (local.get $ptr) (i32.const 49)) (i32.const 114))  ;; 49 r
        (i32.store8 (i32.add (local.get $ptr) (i32.const 50)) (i32.const 97))   ;; 50 a
        (i32.store8 (i32.add (local.get $ptr) (i32.const 51)) (i32.const 109))  ;; 51 m
        (i32.store8 (i32.add (local.get $ptr) (i32.const 52)) (i32.const 101))  ;; 52 e
        (i32.store8 (i32.add (local.get $ptr) (i32.const 53)) (i32.const 116))  ;; 53 t
        (i32.store8 (i32.add (local.get $ptr) (i32.const 54)) (i32.const 101))  ;; 54 e
        (i32.store8 (i32.add (local.get $ptr) (i32.const 55)) (i32.const 114))  ;; 55 r
        (i32.store8 (i32.add (local.get $ptr) (i32.const 56)) (i32.const 115))  ;; 56 s
        (i32.store8 (i32.add (local.get $ptr) (i32.const 57)) (i32.const 34))   ;; 57 "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 58)) (i32.const 58))   ;; 58 :
        (i32.store8 (i32.add (local.get $ptr) (i32.const 59)) (i32.const 123))  ;; 59 {
        (i32.store8 (i32.add (local.get $ptr) (i32.const 60)) (i32.const 34))   ;; 60 "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 61)) (i32.const 116))  ;; 61 t
        (i32.store8 (i32.add (local.get $ptr) (i32.const 62)) (i32.const 121))  ;; 62 y
        (i32.store8 (i32.add (local.get $ptr) (i32.const 63)) (i32.const 112))  ;; 63 p
        (i32.store8 (i32.add (local.get $ptr) (i32.const 64)) (i32.const 101))  ;; 64 e
        (i32.store8 (i32.add (local.get $ptr) (i32.const 65)) (i32.const 34))   ;; 65 "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 66)) (i32.const 58))   ;; 66 :
        (i32.store8 (i32.add (local.get $ptr) (i32.const 67)) (i32.const 34))   ;; 67 "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 68)) (i32.const 111))  ;; 68 o
        (i32.store8 (i32.add (local.get $ptr) (i32.const 69)) (i32.const 98))   ;; 69 b
        (i32.store8 (i32.add (local.get $ptr) (i32.const 70)) (i32.const 106))  ;; 70 j
        (i32.store8 (i32.add (local.get $ptr) (i32.const 71)) (i32.const 101))  ;; 71 e
        (i32.store8 (i32.add (local.get $ptr) (i32.const 72)) (i32.const 99))   ;; 72 c
        (i32.store8 (i32.add (local.get $ptr) (i32.const 73)) (i32.const 116))  ;; 73 t
        (i32.store8 (i32.add (local.get $ptr) (i32.const 74)) (i32.const 34))   ;; 74 "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 75)) (i32.const 44))   ;; 75 ,
        (i32.store8 (i32.add (local.get $ptr) (i32.const 76)) (i32.const 34))   ;; 76 "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 77)) (i32.const 112))  ;; 77 p
        (i32.store8 (i32.add (local.get $ptr) (i32.const 78)) (i32.const 114))  ;; 78 r
        (i32.store8 (i32.add (local.get $ptr) (i32.const 79)) (i32.const 111))  ;; 79 o
        (i32.store8 (i32.add (local.get $ptr) (i32.const 80)) (i32.const 112))  ;; 80 p
        (i32.store8 (i32.add (local.get $ptr) (i32.const 81)) (i32.const 101))  ;; 81 e
        (i32.store8 (i32.add (local.get $ptr) (i32.const 82)) (i32.const 114))  ;; 82 r
        (i32.store8 (i32.add (local.get $ptr) (i32.const 83)) (i32.const 116))  ;; 83 t
        (i32.store8 (i32.add (local.get $ptr) (i32.const 84)) (i32.const 105))  ;; 84 i
        (i32.store8 (i32.add (local.get $ptr) (i32.const 85)) (i32.const 101))  ;; 85 e
        (i32.store8 (i32.add (local.get $ptr) (i32.const 86)) (i32.const 115))  ;; 86 s
        (i32.store8 (i32.add (local.get $ptr) (i32.const 87)) (i32.const 34))   ;; 87 "
        (i32.store8 (i32.add (local.get $ptr) (i32.const 88)) (i32.const 58))   ;; 88 :
        (i32.store8 (i32.add (local.get $ptr) (i32.const 89)) (i32.const 123))  ;; 89 {
        (i32.store8 (i32.add (local.get $ptr) (i32.const 90)) (i32.const 125))  ;; 90 }
        (i32.store8 (i32.add (local.get $ptr) (i32.const 91)) (i32.const 125))  ;; 91 }
        (i32.store8 (i32.add (local.get $ptr) (i32.const 92)) (i32.const 125))  ;; 92 }

        ;; Call register_tool(ptr, 93) — JSON is 93 bytes
        (call $register_tool (local.get $ptr) (i32.const 93))
    )

    (func $tool_handler (export "tool_handler") (param $ptr i32) (param $len i32) (result i32)
        (local $result_ptr i32)
        (local.set $result_ptr (call $alloc (i32.const 19)))
        (i32.store (local.get $result_ptr) (i32.const 15))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 4))  (i32.const 123))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 5))  (i32.const 34))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 6))  (i32.const 114))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 7))  (i32.const 101))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 8))  (i32.const 115))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 9))  (i32.const 117))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 10)) (i32.const 108))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 11)) (i32.const 116))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 12)) (i32.const 34))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 13)) (i32.const 58))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 14)) (i32.const 34))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 15)) (i32.const 111))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 16)) (i32.const 107))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 17)) (i32.const 34))
        (i32.store8 (i32.add (local.get $result_ptr) (i32.const 18)) (i32.const 125))
        (local.get $result_ptr)
    )
)
"#;

#[test]
fn test_minimal_extension_lifecycle() {
    let wasm_bytes = wat::parse_str(MINIMAL_EXTENSION_WAT).expect("WAT should compile");
    let sandbox = pi_extension_system::sandbox::SandboxConfig::default();
    let mut runtime = pi_extension_system::runtime::ExtensionRuntime::new(&wasm_bytes, sandbox)
        .expect("ExtensionRuntime should be created");

    // init
    runtime.call_init().expect("init should succeed");

    // No tools registered by minimal extension
    assert!(runtime.registered_tools().is_empty());

    // Call tool_handler
    let result = runtime.call_tool("test", r#"{}"#).expect("tool_handler should succeed");
    assert_eq!(result, r#"{"result":"ok"}"#);
}

#[test]
fn test_extension_with_tool_registration() {
    let wasm_bytes = wat::parse_str(EXTENSION_WITH_TOOL_WAT).expect("WAT should compile");
    let sandbox = pi_extension_system::sandbox::SandboxConfig::default();
    let mut runtime = pi_extension_system::runtime::ExtensionRuntime::new(&wasm_bytes, sandbox)
        .expect("ExtensionRuntime should be created");

    // init — this extension registers a tool during init
    runtime.call_init().expect("init should succeed");

    // Should have exactly one registered tool
    let tools = runtime.registered_tools();
    assert_eq!(tools.len(), 1, "extension should register one tool");
    assert_eq!(tools[0].name, "my_tool");
    assert_eq!(tools[0].description, "A test tool");

    // Call tool_handler
    let result = runtime.call_tool("my_tool", r#"{}"#).expect("tool_handler should succeed");
    assert_eq!(result, r#"{"result":"ok"}"#);
}
