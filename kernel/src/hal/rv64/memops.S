# memops.S -- rv64 memset/memcpy/memmove
#
# Ported from FreeBSD lib/libc/riscv/string/memset.S and memcpy.S
# SPDX-License-Identifier: BSD-2-Clause
# Copyright (c) 2024 Strahinja Stanisic <strajabot@FreeBSD.org>
#
# Assembly is the compiler's black box — LLVM cannot lower these
# back into calls to themselves, breaking the infinite recursion
# that occurs when implementing memset/memcpy via core::ptr.

# ============================================================
# memset(dst=a0, c=a1, len=a2) -> a0
# ============================================================
.section .text
.globl memset
.type memset, @function
memset:
	andi a1, a1, 0xFF

	sltiu t1, a2, 8
	mv t0, a0
	bnez t1, .Lms_end

	# Replicate byte across 64-bit word without mul:
	# a1 = 0x00000000000000VV -> 0xVVVVVVVVVVVVVVVV
	slli t1, a1, 8
	or a1, a1, t1
	slli t1, a1, 16
	or a1, a1, t1
	slli t1, a1, 32
	or a1, a1, t1

	andi t1, a0, 0b111
	andi t0, a0, ~0b111

	beqz t1, .Lms_loop_store_64

	la t2, .Lms_duff_start
	slli t3, t1, 2
	add t2, t2, t3
	jr -4(t2)
.Lms_duff_start:
	sb a1, 1(t0)
	sb a1, 2(t0)
	sb a1, 3(t0)
	sb a1, 4(t0)
	sb a1, 5(t0)
	sb a1, 6(t0)
	sb a1, 7(t0)

	addi t1, t1, -8
	add a2, a2, t1
	addi t0, t0, 8

.Lms_loop_store_64:
	slti t1, a2, 64
	bnez t1, .Lms_store_rest
	sd a1, 0(t0)
	sd a1, 8(t0)
	sd a1, 16(t0)
	sd a1, 24(t0)
	sd a1, 32(t0)
	sd a1, 40(t0)
	sd a1, 48(t0)
	sd a1, 56(t0)
	addi a2, a2, -64
	addi t0, t0, 64
	j .Lms_loop_store_64

.Lms_store_rest:
	la t2, .Lms_duff_rest
	andi t3, a2, ~0b111
	srli t4, t3, 1
	sub t2, t2, t4
	jr t2
	sd a1, 56(t0)
	sd a1, 48(t0)
	sd a1, 40(t0)
	sd a1, 32(t0)
	sd a1, 24(t0)
	sd a1, 16(t0)
	sd a1, 8(t0)
	sd a1, 0(t0)
.Lms_duff_rest:
	add t0, t0, t3
	sub a2, a2, t3

.Lms_end:
	slli a2, a2, 2
	la t2, .Lms_duff_end
	sub t2, t2, a2
	jr t2
	sb a1, 6(t0)
	sb a1, 5(t0)
	sb a1, 4(t0)
	sb a1, 3(t0)
	sb a1, 2(t0)
	sb a1, 1(t0)
	sb a1, (t0)
.Lms_duff_end:
	ret
.size memset, . - memset

# ============================================================
# memcpy(dst=a0, src=a1, len=a2) -> a0
# ============================================================
.globl memcpy
.type memcpy, @function
memcpy:
	beqz a2, .Lmc_return

	sub t0, a0, a1
	andi t0, t0, 0b111

	sltiu t1, a2, 8

	mv a3, a0

	bnez t1, .Lmc_end

	neg t1, a0
	andi t1, t1, 0b111

	sub a2, a2, t1

	la t2, .Lmc_duff_start
	slli t3, t1, 3
	sub t2, t2, t3
	jr t2
	lb t3, 6(a1)
	sb t3, 6(a3)
	lb t3, 5(a1)
	sb t3, 5(a3)
	lb t3, 4(a1)
	sb t3, 4(a3)
	lb t3, 3(a1)
	sb t3, 3(a3)
	lb t3, 2(a1)
	sb t3, 2(a3)
	lb t3, 1(a1)
	sb t3, 1(a3)
	lb t3, 0(a1)
	sb t3, 0(a3)
.Lmc_duff_start:

	add a1, a1, t1
	add a3, a3, t1

	beqz a2, .Lmc_return

	beqz t0, .Lmc_memcpy8

	andi a4, a1, 0b111
	slli a4, a4, 3

	neg a5, a4

	srli a6, a2, 3

	andi a2, a2, 0b111

	andi t0, a1, ~0b111

	slli t1, a6, 3
	add a1, a1, t1

	ld t1, (t0)
	addi t0, t0, 8

	srl t1, t1, a4

	beqz a6, .Lmc_lmain_skip
.Lmc_lmain:
	ld t2, (t0)
	addi t0, t0, 8

	addi a6, a6, -1

	sll t3, t2, a5
	or t3, t3, t1

	srl t1, t2, a4

	sd t3, (a3)
	addi a3, a3, 8

	bnez a6, .Lmc_lmain

.Lmc_lmain_skip:

.Lmc_end:
	la t1, .Lmc_duff_end
	slli t2, a2, 3
	sub t1, t1, t2
	jr t1
	lb t2, 6(a1)
	sb t2, 6(a3)
	lb t2, 5(a1)
	sb t2, 5(a3)
	lb t2, 4(a1)
	sb t2, 4(a3)
	lb t2, 3(a1)
	sb t2, 3(a3)
	lb t2, 2(a1)
	sb t2, 2(a3)
	lb t2, 1(a1)
	sb t2, 1(a3)
	lb t2, 0(a1)
	sb t2, 0(a3)
.Lmc_duff_end:

.Lmc_return:
	ret

.Lmc_memcpy8:
	beqz a2, .Lmc_return

	slti t0, a2, 128
	bnez t0, .Lmc_lmain8_64_skip

	andi t0, a2, ~0b111111
	add a4, a3, t0

	andi a2, a2, 0b111111

.Lmc_lmain8_64:
	ld t0, 0(a1)
	ld t1, 8(a1)
	ld t2, 16(a1)
	ld t3, 24(a1)
	sd t0, 0(a3)
	sd t1, 8(a3)
	sd t2, 16(a3)
	sd t3, 24(a3)
	ld t0, 32(a1)
	ld t1, 40(a1)
	ld t2, 48(a1)
	ld t3, 56(a1)
	sd t0, 32(a3)
	sd t1, 40(a3)
	sd t2, 48(a3)
	sd t3, 56(a3)
	addi a3, a3, 64
	addi a1, a1, 64
	bne a3, a4, .Lmc_lmain8_64
.Lmc_lmain8_64_skip:

	beqz a2, .Lmc_return

	add a4, a3, a2
	andi a4, a4, ~0b111

	andi a2, a2, 0b111

	beq a3, a4, .Lmc_lmain8_skip
.Lmc_lmain8:
	ld t0, (a1)
	sd t0, (a3)
	addi a3, a3, 8
	addi a1, a1, 8
	bne a3, a4, .Lmc_lmain8
.Lmc_lmain8_skip:

	la t1, .Lmc_duff_end
	slli t2, a2, 3
	sub t1, t1, t2
	jr t1
.size memcpy, . - memcpy

# ============================================================
# memmove(dst=a0, src=a1, len=a2) -> a0
#
# FreeBSD uses generic C for memmove on rv64. We provide a
# simple asm version: forward copy if dst < src, backward
# copy if dst > src, to handle overlapping regions.
# ============================================================
.globl memmove
.type memmove, @function
memmove:
	beqz a2, .Lmm_return
	beq a0, a1, .Lmm_return

	# If dst < src, forward copy (same as memcpy)
	bltu a0, a1, memcpy

	# dst > src: check if they actually overlap
	add t0, a1, a2
	bleu t0, a0, memcpy    # no overlap, forward is fine

	# Overlapping, dst > src: backward byte copy
	add a3, a0, a2         # a3 = dst + len
	add a4, a1, a2         # a4 = src + len
.Lmm_backward:
	addi a3, a3, -1
	addi a4, a4, -1
	lb t0, (a4)
	sb t0, (a3)
	bne a3, a0, .Lmm_backward

.Lmm_return:
	ret
.size memmove, . - memmove
