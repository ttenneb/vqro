(module
  (import "cm32p2|vqro:extension/host@1" "call" (func $host-call (param i32 i32 i32)))
  (import "cm32p2|vqro:extension/host@1" "log" (func $host-log (param i32 i32 i32)))
  (import "cm32p2|vqro:extension/host@1" "cancelled" (func $host-cancelled (result i32)))

  (memory (export "cm32p2_memory") 8 1024)
  (global $heap (mut i32) (i32.const 8192))
  (global $calls (mut i32) (i32.const 0))

  ;; Descriptor result: ok({ service-id: "echo", methods: conformance vectors }).
  (data (i32.const 100) "echo")
  (data (i32.const 200) "conformance.echo")
  (data (i32.const 220) "conformance.document")
  (data (i32.const 300) "{\22count\22:2,\22text\22:\22vqro\22}")
  (data (i32.const 340) "[]")
  (data (i32.const 360) "not-json")
  (data (i32.const 380) "\22denied\22")
  (data (i32.const 400) "{\22document_id\22:\22fixture\22,\22nodes\22:[]}")
  (data (i32.const 500) "{\22call_id\22:\22call-1\22,\22capability\22:\22state\22,\22depth\22:1,\22identity\22:{\22generation\22:26,\22namespace\22:\22example.component\22,\22package_id\22:\22example.component\22,\22service_id\22:\22echo\22},\22method\22:\22state.snapshot\22,\22namespace\22:\22example.component\22,\22params\22:{},\22type\22:\22host_call\22}")

  (func (export "cm32p2_realloc") (param $old i32) (param $old-len i32)
        (param $align i32) (param $new-len i32) (result i32)
    (local $result i32)
    global.get $heap
    local.get $align
    i32.const 1
    i32.sub
    i32.add
    local.get $align
    i32.const 1
    i32.sub
    i32.const -1
    i32.xor
    i32.and
    local.tee $result
    local.get $new-len
    i32.add
    global.set $heap
    local.get $result)

  (func (export "cm32p2_initialize"))

  (func (export "cm32p2||descriptor") (result i32)
    i32.const 32
    i32.const 0
    i32.store8
    i32.const 36
    i32.const 100
    i32.store
    i32.const 40
    i32.const 4
    i32.store
    i32.const 44
    i32.const 160
    i32.store
    i32.const 48
    i32.const 2
    i32.store
    i32.const 160
    i32.const 200
    i32.store
    i32.const 164
    i32.const 16
    i32.store
    i32.const 168
    i32.const 220
    i32.store
    i32.const 172
    i32.const 20
    i32.store
    i32.const 32)
  (func (export "cm32p2||descriptor_post") (param i32))

  (func (export "cm32p2||invoke") (param $request i32) (param $request-len i32) (result i32)
    ;; INVOKE_BODY
  )
  (func (export "cm32p2||invoke_post") (param i32))
)
