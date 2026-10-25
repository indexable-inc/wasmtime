;;! target = "x86_64"
;;! flags = "-W gc,exceptions"
;;! test = "compile"
;;! objdump = "--stack-maps=true"

(module
  (type $s (struct (field (mut i32))))

  (import "" "gc" (func $gc))

  (func (export "run") (result i32)
    (struct.new $s (i32.const 42))

    block $b
      try_table (catch_all $b)
        ;; This should have both exception handlers and stack maps in the
        ;; disassembly below.
        call $gc
      end
    end

    struct.get $s 0
  )
)
;; wasm[0]::function[1]:
;;       pushq   %rbp
;;       movq    %rsp, %rbp
;;       movq    8(%rdi), %r10
;;       movq    0x18(%r10), %r10
;;       addq    $0x50, %r10
;;       cmpq    %rsp, %r10
;;       ja      0x111
;;   19: subq    $0x40, %rsp
;;       movq    %rbx, 0x10(%rsp)
;;       movq    %r12, 0x18(%rsp)
;;       movq    %r13, 0x20(%rsp)
;;       movq    %r14, 0x28(%rsp)
;;       movq    %r15, 0x30(%rsp)
;;       movq    0x28(%rdi), %rcx
;;       movl    (%rcx), %eax
;;       movl    4(%rcx), %edx
;;       leaq    0x20(%rax), %rsi
;;       cmpq    %rdx, %rsi
;;       ja      0xdd
;;   4c: movq    %rdi, 8(%rsp)
;;       leal    0x20(%rax), %esi
;;       movl    %esi, (%rcx)
;;       movq    8(%rsp), %r8
;;       movq    8(%r8), %rcx
;;       movq    0x20(%rcx), %rsi
;;       leaq    (%rsi, %rax), %rcx
;;       movl    $0xb0000002, (%rsi, %rax)
;;       ╰─╼ trap: GcHeapCorrupt
;;       movq    0x30(%r8), %rdi
;;       movl    (%rdi), %edi
;;       movl    %edi, 4(%rsi, %rax)
;;       ╰─╼ trap: GcHeapCorrupt
;;       movl    $0x20, %edi
;;       movl    %edi, 8(%rsi, %rax)
;;       ╰─╼ trap: GcHeapCorrupt
;;       movl    %eax, (%rsp)
;;       movl    $0x2a, 0x10(%rcx)
;;       ╰─╼ trap: GcHeapCorrupt
;;       movq    8(%rsp), %r8
;;       movq    0x48(%r8), %rax
;;       movq    0x58(%r8), %rdi
;;       movq    8(%rsp), %rsi
;;       callq   *%rax
;;       ├─╼ exception frame offset: SP = FP - 0x40
;;       ╰─╼ exception handler: default handler, context at [SP+0x8], handler=0x9f
;;       movl    (%rsp), %eax
;;       ╰─╼ stack_map: frame_size=64, frame_offsets=[0]
;;       testl   %eax, %eax
;;       je      0x113
;;   aa: movq    8(%rsp), %r8
;;       movq    8(%r8), %rcx
;;       movq    0x20(%rcx), %rcx
;;       movl    0x10(%rcx, %rax), %eax
;;       ╰─╼ trap: GcHeapCorrupt
;;       movq    0x10(%rsp), %rbx
;;       movq    0x18(%rsp), %r12
;;       movq    0x20(%rsp), %r13
;;       movq    0x28(%rsp), %r14
;;       movq    0x30(%rsp), %r15
;;       addq    $0x40, %rsp
;;       movq    %rbp, %rsp
;;       popq    %rbp
;;       retq
;;   dd: movl    $0xb0000002, %esi
;;   e2: movq    0x30(%rdi), %rax
;;   e6: movq    %rdi, 8(%rsp)
;;   eb: movl    (%rax), %edx
;;   ed: movl    $0x20, %ecx
;;   f2: movl    $0x10, %r8d
;;   f8: callq   0x288
;;   fd: movq    8(%rsp), %r8
;;  102: movq    8(%r8), %rsi
;;  106: movl    %eax, %ecx
;;  108: addq    0x20(%rsi), %rcx
;;  10c: jmp     0x81
;;  111: ud2
;;       ╰─╼ trap: Normal(StackOverflow)
;;  113: ud2
;;       ╰─╼ trap: Normal(NullReference)
