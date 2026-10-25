;;! target = "x86_64-unknown-linux-gnu"
;;! test = "compile"
;;! flags = "-Ccompiler=cranelift -Ccranelift-has_sse41=false  -Osignals-based-traps=n"

(module
  (func (export "i64.trunc_f64_u") (param f64) (result i64)
    (i64.trunc_f64_u (local.get 0))
  )
)
;; wasm[0]::function[0]:
;;       pushq   %rbp
;;       movq    %rsp, %rbp
;;       subq    $0x20, %rsp
;;       movq    %rbx, 0x10(%rsp)
;;       movq    8(%rdi), %rax
;;       movq    %rdi, %rbx
;;       movq    0x18(%rax), %rax
;;       movq    %rsp, %rcx
;;       cmpq    %rax, %rcx
;;       jb      0x10d
;;   24: ucomisd %xmm0, %xmm0
;;       movdqu  %xmm0, (%rsp)
;;       jp      0xf9
;;       jne     0xf9
;;   39: callq   0x264
;;       movabsq $13830554455654793216, %rax
;;       movq    %rax, %xmm1
;;       ucomisd %xmm0, %xmm1
;;       jae     0xe2
;;   57: ucomisd 0xd1(%rip), %xmm0
;;       jae     0xcb
;;   65: movdqu  (%rsp), %xmm2
;;       movabsq $0x43e0000000000000, %rcx
;;       movq    %rcx, %xmm1
;;       ucomisd %xmm1, %xmm2
;;       jae     0x9a
;;       jp      0x11e
;;   89: cvttsd2si %xmm2, %rax
;;       cmpq    $0, %rax
;;       jge     0xbd
;;   98: ud2
;;       movaps  %xmm2, %xmm0
;;       subsd   %xmm1, %xmm0
;;       cvttsd2si %xmm0, %rax
;;       cmpq    $0, %rax
;;       jl      0x120
;;   b0: movabsq $9223372036854775808, %rcx
;;       addq    %rcx, %rax
;;       movq    0x10(%rsp), %rbx
;;       addq    $0x20, %rsp
;;       movq    %rbp, %rsp
;;       popq    %rbp
;;       retq
;;   cb: movl    $6, %esi
;;   d0: movq    %rbx, %rdi
;;   d3: callq   0x291
;;   d8: movq    %rbx, %rdi
;;   db: callq   0x2c2
;;   e0: ud2
;;   e2: movl    $6, %esi
;;   e7: movq    %rbx, %rdi
;;   ea: callq   0x291
;;   ef: movq    %rbx, %rdi
;;   f2: callq   0x2c2
;;   f7: ud2
;;   f9: movl    $8, %esi
;;   fe: callq   0x291
;;  103: movq    %rbx, %rdi
;;  106: callq   0x2c2
;;  10b: ud2
;;  10d: xorl    %esi, %esi
;;  10f: callq   0x291
;;  114: movq    %rbx, %rdi
;;  117: callq   0x2c2
;;  11c: ud2
;;  11e: ud2
;;  120: ud2
;;  122: addb    %al, (%rax)
;;  124: addb    %al, (%rax)
;;  126: addb    %al, (%rax)
;;  128: addb    %al, (%rax)
;;  12a: addb    %al, (%rax)
;;  12c: addb    %al, (%rax)
;;  12e: addb    %al, (%rax)
;;  130: addb    %al, (%rax)
;;  132: addb    %al, (%rax)
;;  134: addb    %al, (%rax)
;;  136: lock addb %al, (%r8)
;;  13a: addb    %al, (%rax)
;;  13c: addb    %al, (%rax)
;;  13e: addb    %al, (%rax)
