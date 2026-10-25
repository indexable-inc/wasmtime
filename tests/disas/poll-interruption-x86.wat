;;! target = "x86_64"
;;! test = "compile"
;;! flags = ["-Wpoll-interruption=y"]
;;! objdump = "--traps=true"

;; Poll interruption (ix fork): exactly one poll at function entry and
;; one per loop iteration. A poll is a plain load of the store's poll word
;; and a branch to a trap instruction carrying the `interrupt` trap code;
;; no fence, and no load carries that code. `count` has a loop; `leaf` has
;; none and polls only at entry.
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
;;       testq   %r10, %r10
;;       jne     0x35
;;   14: xorl    %eax, %eax
;;       movq    (%r9), %rcx
;;       testq   %rcx, %rcx
;;       jne     0x37
;;   22: addl    $1, %eax
;;       cmpl    %edx, %eax
;;       jb      0x16
;;   30: movq    %rbp, %rsp
;;       popq    %rbp
;;       retq
;;   35: ud2
;;       ╰─╼ trap: Normal(Interrupt)
;;   37: ud2
;;       ╰─╼ trap: Normal(Interrupt)
;;
;; wasm[0]::function[1]::leaf:
;;       pushq   %rbp
;;       movq    %rsp, %rbp
;;       movq    0x20(%rdi), %rsi
;;       movq    (%rsi), %rsi
;;       testq   %rsi, %rsi
;;       jne     0x5e
;;   54: movl    $7, %eax
;;       movq    %rbp, %rsp
;;       popq    %rbp
;;       retq
;;   5e: ud2
;;       ╰─╼ trap: Normal(Interrupt)
