;;! target = "x86_64"
;;! test = "compile"
;;! flags = ["-Wexceptions=yes", "-Wgc=yes"]

(module
 (tag $e0 (param i32 i64))

 (func $throw (param i32 i64)
       (throw $e0 (local.get 0) (local.get 1)))

 (func $catch (export "catch") (param i32 i64) (result i32 i64)

       (block $b (result i32 i64)
              (try_table (result i32 i64)
                         (catch $e0 $b)
                         (call $throw (local.get 0) (local.get 1))
                         (i32.const 42)
                         (i64.const 100)))))
;; wasm[0]::function[0]::throw:
;;       pushq   %rbp
;;       movq    %rsp, %rbp
;;       movq    8(%rdi), %r10
;;       movq    0x18(%r10), %r10
;;       addq    $0x50, %r10
;;       cmpq    %rsp, %r10
;;       ja      0xea
;;   19: subq    $0x40, %rsp
;;       movq    %rbx, 0x10(%rsp)
;;       movq    %r12, 0x18(%rsp)
;;       movq    %r13, 0x20(%rsp)
;;       movq    %r14, 0x28(%rsp)
;;       movq    %r15, 0x30(%rsp)
;;       movq    %rdi, %rbx
;;       movq    %rcx, %r13
;;       movq    %rdx, %r14
;;       callq   0x409
;;       movq    %rax, %r12
;;       movq    0x28(%rbx), %rcx
;;       movl    (%rcx), %esi
;;       movl    4(%rcx), %eax
;;       leaq    0x30(%rsi), %rdx
;;       cmpq    %rax, %rdx
;;       ja      0xb4
;;   5d: leal    0x30(%rsi), %eax
;;       movl    %eax, (%rcx)
;;       movq    8(%rbx), %rax
;;       movq    0x20(%rax), %rdx
;;       leaq    (%rdx, %rsi), %rcx
;;       movl    $0x4000002, (%rdx, %rsi)
;;       movq    0x30(%rbx), %rax
;;       movl    0xc(%rax), %eax
;;       movl    %eax, 4(%rdx, %rsi)
;;       movl    $0x30, %eax
;;       movl    %eax, 8(%rdx, %rsi)
;;       movq    %r14, %rdx
;;       movl    %edx, 0x18(%rcx)
;;       movq    %r13, %rax
;;       movq    %rax, 0x20(%rcx)
;;       movq    %r12, %rax
;;       movl    %eax, 0x10(%rcx)
;;       movl    $0, 0x14(%rcx)
;;       movq    %rbx, %rdi
;;       movq    %rbx, (%rsp)
;;       callq   0x436
;;       jmp     0xe8
;;   b4: movl    $0x4000002, %esi
;;       movq    0x30(%rbx), %rax
;;       movl    0xc(%rax), %edx
;;       movl    $0x30, %ecx
;;       movl    $0x10, %r8d
;;       movq    %rbx, %rdi
;;       callq   0x3a0
;;       movq    8(%rbx), %rdx
;;       movl    %eax, %ecx
;;       addq    0x20(%rdx), %rcx
;;       movq    %r14, %rdx
;;       movq    %rax, %rsi
;;       jmp     0x8c
;;   e8: ud2
;;       ud2
;;
;; wasm[0]::function[1]::catch:
;;       pushq   %rbp
;;       movq    %rsp, %rbp
;;       movq    8(%rdi), %r10
;;       movq    0x18(%r10), %r10
;;       addq    $0x50, %r10
;;       cmpq    %rsp, %r10
;;       ja      0x191
;;  119: subq    $0x40, %rsp
;;       movq    %rbx, 0x10(%rsp)
;;       movq    %r12, 0x18(%rsp)
;;       movq    %r13, 0x20(%rsp)
;;       movq    %r14, 0x28(%rsp)
;;       movq    %r15, 0x30(%rsp)
;;       movq    %rdi, (%rsp)
;;       movq    (%rsp), %rsi
;;       callq   0
;;       ├─╼ exception frame offset: SP = FP - 0x40
;;       ╰─╼ exception handler: tag=0, context at [SP+0x0], handler=0x152
;;       movl    $0x2a, %eax
;;       movl    $0x64, %ecx
;;       jmp     0x16f
;;  152: movq    %rax, %rdx
;;       movq    (%rsp), %rsi
;;       movq    8(%rsi), %rax
;;       movq    0x20(%rax), %rcx
;;       movq    %rdx, %rax
;;       movl    %eax, %edx
;;       movl    0x18(%rcx, %rdx), %eax
;;       movq    0x20(%rcx, %rdx), %rcx
;;       movq    0x10(%rsp), %rbx
;;       movq    0x18(%rsp), %r12
;;       movq    0x20(%rsp), %r13
;;       movq    0x28(%rsp), %r14
;;       movq    0x30(%rsp), %r15
;;       addq    $0x40, %rsp
;;       movq    %rbp, %rsp
;;       popq    %rbp
;;       retq
;;  191: ud2
