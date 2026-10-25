;;! target = "x86_64"
;;! test = "compile"
;;! flags = ["-Wpoll-interruption=y"]
;;! objdump = "--traps=true"

;; Poll-page interruption (ix fork): exactly one poll (a load that carries
;; the `interrupt` trap code) at function entry and one per loop
;; iteration, with no compare and no branch. `count` has a loop; `leaf`
;; has none and polls only at entry.
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
;;       movq    0x20(%rdi), %r9
;;       movq    (%r9), %r10
;;       ╰─╼ trap: Normal(Interrupt)
;;       xorl    %eax, %eax
;;       movq    (%r9), %r10
;;       ╰─╼ trap: Normal(Interrupt)
;;       addl    $1, %eax
;;       cmpl    %edx, %eax
;;       jb      0xd
;;   1e: movq    %rbp, %rsp
;;       popq    %rbp
;;       retq
;;
;; wasm[0]::function[1]::leaf:
;;       pushq   %rbp
;;       movq    %rsp, %rbp
;;       movq    0x20(%rdi), %rsi
;;       movq    (%rsi), %rsi
;;       ╰─╼ trap: Normal(Interrupt)
;;       movl    $7, %eax
;;       movq    %rbp, %rsp
;;       popq    %rbp
;;       retq
