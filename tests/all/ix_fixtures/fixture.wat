(module
  (type $ii (func (param i32) (result i32)))
  (table 2 funcref)
  (elem (i32.const 0) $inc $inc)
  (func $inc (type $ii) (i32.add (local.get 0) (i32.const 1)))
  (func (export "call") (param i32 i32) (result i32)
    (call_indirect (type $ii) (local.get 1) (local.get 0))))
