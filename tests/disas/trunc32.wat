;;! target = "x86_64-unknown-linux-gnu"
;;! test = "compile"
;;! flags = "-Ccompiler=cranelift -Ccranelift-has_sse41=false  -Osignals-based-traps=n"

(module
  (func (export "i32.trunc_f32_u") (param f32) (result i32)
    (i32.trunc_f32_u (local.get 0))
  )
)
;; wasm[0]::function[0]:
;;       pushq   %rbp
;;       movq    %rsp, %rbp
;;       subq    $0x20, %rsp
;;       movq    %rbx, 0x10(%rsp)
;;       movdqu  %xmm0, (%rsp)
;;       movq    8(%rdi), %rax
;;       movq    %rdi, %rbx
;;       movq    0x18(%rax), %rax
;;       movq    %rsp, %rcx
;;       cmpq    %rax, %rcx
;;       jb      0x107
;;   29: xorpd   %xmm0, %xmm0
;;       movdqu  (%rsp), %xmm1
;;       cvtss2sd %xmm1, %xmm0
;;       ucomisd %xmm0, %xmm0
;;       jp      0xf3
;;       jne     0xf3
;;   46: callq   0x252
;;       movabsq $13830554455654793216, %rax
;;       movq    %rax, %xmm1
;;       ucomisd %xmm0, %xmm1
;;       jae     0xdc
;;   64: ucomisd 0xb4(%rip), %xmm0
;;       jae     0xc5
;;   72: movdqu  (%rsp), %xmm2
;;       movl    $0x4f000000, %ecx
;;       movd    %ecx, %xmm1
;;       ucomiss %xmm1, %xmm2
;;       jae     0x9e
;;       jp      0x118
;;   8f: cvttss2si %xmm2, %eax
;;       cmpl    $0, %eax
;;       jge     0xb7
;;   9c: ud2
;;       movaps  %xmm2, %xmm0
;;       subss   %xmm1, %xmm0
;;       cvttss2si %xmm0, %eax
;;       cmpl    $0, %eax
;;       jl      0x11a
;;   b2: addl    $0x80000000, %eax
;;       movq    0x10(%rsp), %rbx
;;       addq    $0x20, %rsp
;;       movq    %rbp, %rsp
;;       popq    %rbp
;;       retq
;;   c5: movl    $6, %esi
;;   ca: movq    %rbx, %rdi
;;   cd: callq   0x27f
;;   d2: movq    %rbx, %rdi
;;   d5: callq   0x2b0
;;   da: ud2
;;   dc: movl    $6, %esi
;;   e1: movq    %rbx, %rdi
;;   e4: callq   0x27f
;;   e9: movq    %rbx, %rdi
;;   ec: callq   0x2b0
;;   f1: ud2
;;   f3: movl    $8, %esi
;;   f8: callq   0x27f
;;   fd: movq    %rbx, %rdi
;;  100: callq   0x2b0
;;  105: ud2
;;  107: xorl    %esi, %esi
;;  109: callq   0x27f
;;  10e: movq    %rbx, %rdi
;;  111: callq   0x2b0
;;  116: ud2
;;  118: ud2
;;  11a: ud2
;;  11c: addb    %al, (%rax)
;;  11e: addb    %al, (%rax)
;;  120: addb    %al, (%rax)
;;  122: addb    %al, (%rax)
;;  124: addb    %al, (%rax)
;;  126: lock addb %al, (%r8)
;;  12a: addb    %al, (%rax)
;;  12c: addb    %al, (%rax)
;;  12e: addb    %al, (%rax)
