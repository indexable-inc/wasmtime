;;! target = "x86_64"
;;! test = "compile"
;;! flags = ["-Wpoll-interruption=y"]
;;! objdump = "--traps=true"

;; Poll interruption (ix fork): one poll per loop iteration. A poll is a
;; plain load of the store's poll word and a branch to a trap instruction
;; carrying the `interrupt` trap code; no fence, and no load carries that
;; code. Both functions are bounded (`IxPollPlan`): `count` polls only at
;; its loop header and `leaf` (no loop, no call) does not poll at all; its
;; caller or the host-to-wasm entry polled before it. On x86 the
;; poll is `cmpq $0, (ptr)`: the word is compared in memory.
(module
  (func $count (param i32) (result i32) (local i32)
    (loop
      (local.set 1 (i32.add (local.get 1) (i32.const 1)))
      (br_if 0 (i32.lt_u (local.get 1) (local.get 0))))
    (local.get 1))
  (func $leaf (result i32) (i32.const 7)))
;; wasm[0]::function[0]::count:
;;       pushq   %rbp
;;       movq    %rsp, %rbp
;;       xorl    %eax, %eax
;;       movq    0x20(%rdi), %rdi
;;       cmpq    $0, (%rdi)
;;       jne     0x27
;;   14: addl    $1, %eax
;;       cmpl    %edx, %eax
;;       jb      0xa
;;   22: movq    %rbp, %rsp
;;       popq    %rbp
;;       retq
;;   27: ud2
;;       ╰─╼ trap: Normal(Interrupt)
;;
;; wasm[0]::function[1]::leaf:
;;       pushq   %rbp
;;       movq    %rsp, %rbp
;;       movl    $7, %eax
;;       movq    %rbp, %rsp
;;       popq    %rbp
;;       retq
