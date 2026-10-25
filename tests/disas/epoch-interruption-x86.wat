;;! target = "x86_64"
;;! test = "compile"
;;! flags = ["-Wepoch-interruption=y"]

(module (func (loop (br 0))))

;; wasm[0]::function[0]:
;;       pushq   %rbp
;;       movq    %rsp, %rbp
;;       movq    8(%rdi), %r10
;;       movq    0x18(%r10), %r10
;;       addq    $0x30, %r10
;;       cmpq    %rsp, %r10
;;       ja      0x7f
;;   19: subq    $0x20, %rsp
;;       movq    %r12, (%rsp)
;;       movq    %r13, 8(%rsp)
;;       movq    %r14, 0x10(%rsp)
;;       movq    0x18(%rdi), %r13
;;       movq    (%r13), %rcx
;;       movq    8(%rdi), %r12
;;       movq    %rdi, %r14
;;       movq    8(%r12), %rax
;;       cmpq    %rax, %rcx
;;       jae     0x5a
;;   48: movq    (%r13), %rcx
;;       cmpq    %rax, %rcx
;;       jae     0x64
;;       jmp     0x48
;;   5a: callq   0xdb
;;       jmp     0x48
;;   64: movq    8(%r12), %rax
;;       cmpq    %rax, %rcx
;;       jb      0x48
;;   72: movq    %r14, %rdi
;;       callq   0xdb
;;       jmp     0x48
;;   7f: ud2
