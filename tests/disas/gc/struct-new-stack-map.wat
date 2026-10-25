;;! target = "x86_64"
;;! flags = "-W function-references,gc"
;;! test = "compile"

(module
  (type $ty (struct (field (mut f32))
                    (field (mut i8))
                    (field (mut anyref))))

  (func (param f32 i32 anyref) (result (ref $ty))
    (struct.new $ty (local.get 0) (local.get 1) (local.get 2))
  )
)
;; wasm[0]::function[0]:
;;       pushq   %rbp
;;       movq    %rsp, %rbp
;;       movq    8(%rdi), %r10
;;       movq    0x18(%r10), %r10
;;       addq    $0x40, %r10
;;       cmpq    %rsp, %r10
;;       ja      0xce
;;   19: subq    $0x30, %rsp
;;       movq    %rbx, 0x20(%rsp)
;;       movq    %r12, 0x28(%rsp)
;;       movq    %rdx, %r12
;;       movdqu  %xmm0, 8(%rsp)
;;       movl    %ecx, (%rsp)
;;       movq    0x28(%rdi), %rcx
;;       movl    (%rcx), %eax
;;       movl    4(%rcx), %edx
;;       leaq    0x20(%rax), %rsi
;;       cmpq    %rdx, %rsi
;;       ja      0x9b
;;   49: movq    %rdi, %rbx
;;       leal    0x20(%rax), %edx
;;       movl    %edx, (%rcx)
;;       movq    8(%rbx), %rcx
;;       movq    0x20(%rcx), %rdx
;;       leaq    (%rdx, %rax), %rcx
;;       movl    $0xb0000022, (%rdx, %rax)
;;       movq    0x30(%rbx), %rsi
;;       movl    (%rsi), %esi
;;       movl    %esi, 4(%rdx, %rax)
;;       movl    $0x20, %esi
;;       movl    %esi, 8(%rdx, %rax)
;;       movss   %xmm0, 0x10(%rcx)
;;       movq    %r12, %rdx
;;       movb    %dl, 0x14(%rcx)
;;       movl    (%rsp), %edx
;;       movl    %edx, 0x18(%rcx)
;;       movq    0x20(%rsp), %rbx
;;       movq    0x28(%rsp), %r12
;;       addq    $0x30, %rsp
;;       movq    %rbp, %rsp
;;       popq    %rbp
;;       retq
;;   9b: movl    $0xb0000022, %esi
;;   a0: movq    0x30(%rdi), %rax
;;   a4: movq    %rdi, %rbx
;;   a7: movl    (%rax), %edx
;;   a9: movl    $0x20, %ecx
;;   ae: movl    $0x10, %r8d
;;   b4: callq   0x144
;;   b9: movq    8(%rbx), %rdx
;;       ╰─╼ stack_map: frame_size=48, frame_offsets=[0]
;;   bd: movl    %eax, %ecx
;;   bf: addq    0x20(%rdx), %rcx
;;   c3: movdqu  8(%rsp), %xmm0
;;   c9: jmp     0x77
;;   ce: ud2
