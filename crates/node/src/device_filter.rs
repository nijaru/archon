//! Process-side device enforcement: a cgroup-device BPF filter attached to
//! the lease's cgroup. Deny-by-default — the lease may access exactly its
//! claimed devices plus a fixed set of standard pseudo-devices; everything
//! else returns EPERM. This is what makes device claims real for bare
//! processes on Linux, mirroring how containers enforce `--device`.

#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use crate::protocol::DeviceAccess;

// eBPF instruction classes and ops we emit (linux/bpf.h).
const BPF_LDX_MEM_W: u8 = 0x61; // ldxw dst, [src+off]
const BPF_ALU32_MOV_IMM: u8 = 0xb4;
const BPF_ALU32_AND_IMM: u8 = 0x54;
const BPF_ALU32_RSH_IMM: u8 = 0x74;
const BPF_JMP32_JNE_IMM: u8 = 0x56;
const BPF_JA: u8 = 0x05;
const BPF_EXIT: u8 = 0x95;

// BPF commands.
const BPF_PROG_LOAD: libc::c_int = 5;
const BPF_PROG_ATTACH: libc::c_int = 8;

// Program types / attach types (linux/bpf.h).
const BPF_PROG_TYPE_CGROUP_DEVICE: libc::c_int = 15;
const BPF_CGROUP_DEVICE: libc::c_int = 6;

// bpf_cgroup_dev_ctx access bits (linux/bpf.h).
const DEVCG_ACC_READ: u32 = 1;
const DEVCG_ACC_WRITE: u32 = 2;
const DEVCG_ACC_MKNOD: u32 = 4;
// Device type lives in the upper 16 bits of ctx->access_type.
const DEVCG_DEV_BLOCK: u32 = 1;
const DEVCG_DEV_CHAR: u32 = 2;

/// One allowed (device type, major, minor) with its granted access bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceRule {
    dev_type: u32,
    major: u32,
    minor: u32,
    /// Granted bits out of READ | WRITE | MKNOD.
    access: u32,
}

/// A raw eBPF instruction, kept in field order for direct syscall use.
#[derive(Clone, Copy)]
pub(crate) struct Insn {
    code: u8,
    dst_reg: u8,
    src_reg: u8,
    off: i16,
    imm: i32,
}

fn mov_imm(dst: u8, imm: i32) -> Insn {
    Insn {
        code: BPF_ALU32_MOV_IMM,
        dst_reg: dst,
        src_reg: 0,
        off: 0,
        imm,
    }
}

fn mov_reg(dst: u8, src: u8) -> Insn {
    Insn {
        code: 0xbf,
        dst_reg: dst,
        src_reg: src,
        off: 0,
        imm: 0,
    }
}

fn and_imm(dst: u8, imm: u32) -> Insn {
    Insn {
        code: BPF_ALU32_AND_IMM,
        dst_reg: dst,
        src_reg: 0,
        off: 0,
        imm: imm as i32,
    }
}

fn rsh_imm(dst: u8, imm: u32) -> Insn {
    Insn {
        code: BPF_ALU32_RSH_IMM,
        dst_reg: dst,
        src_reg: 0,
        off: 0,
        imm: imm as i32,
    }
}

fn load_ctx_word(dst: u8, byte_off: u16) -> Insn {
    Insn {
        code: BPF_LDX_MEM_W,
        dst_reg: dst,
        src_reg: 1,
        off: byte_off as i16,
        imm: 0,
    }
}

/// Assemble the deny-by-default cgroup-device program:
///
/// ```text
/// r2 = ctx->access_type; r3 = ctx->major; r4 = ctx->minor
/// for each rule:
///     if (access_type & 0xffff) & ~rule.access == 0        // request ⊆ grant
///     && (access_type >> 16) == rule.dev_type
///     && major == rule.major && minor == rule.minor:
///         return 1
/// return 0
/// ```
///
/// Every mismatch jumps to the next rule block; a full match jumps to the
/// shared ALLOW epilogue. Empty rules compile to an unconditional deny.
pub(crate) fn compile(rules: &[DeviceRule]) -> Vec<Insn> {
    // Deny-by-default with no rules is just an unconditional reject.
    if rules.is_empty() {
        return vec![
            mov_imm(0, 0),
            Insn {
                code: BPF_EXIT,
                dst_reg: 0,
                src_reg: 0,
                off: 0,
                imm: 0,
            },
        ];
    }

    let mut prog = vec![
        load_ctx_word(2, 0), // access_type
        load_ctx_word(3, 4), // major
        load_ctx_word(4, 8), // minor
    ];

    // Jump offsets are relative to the instruction AFTER the jump itself.
    let jump_to = |from: usize, target: usize| (target - from - 1) as i16;

    let allow = prog.len() + rules.len() * 10;
    let deny = allow + 2;
    for (i, rule) in rules.iter().enumerate() {
        let s = prog.len();
        let next_rule = if i + 1 < rules.len() { s + 10 } else { deny };
        let granted = rule.access & (DEVCG_ACC_READ | DEVCG_ACC_WRITE | DEVCG_ACC_MKNOD);
        let disallowed = !granted;
        // s+0..1: r6 = requested access flags.
        prog.push(mov_reg(6, 2)); // r6 = access_type
        prog.push(and_imm(6, 0xffff));
        // s+2..3: any requested bit outside the grant means "not this rule".
        prog.push(and_imm(6, disallowed));
        prog.push(Insn {
            code: BPF_JMP32_JNE_IMM,
            dst_reg: 6,
            src_reg: 0,
            off: jump_to(s + 3, next_rule),
            imm: 0,
        });
        // s+4..6: device type from the upper half-word must match exactly.
        prog.push(mov_reg(6, 2));
        prog.push(rsh_imm(6, 16));
        prog.push(Insn {
            code: BPF_JMP32_JNE_IMM,
            dst_reg: 6,
            src_reg: 0,
            off: jump_to(s + 6, next_rule),
            imm: rule.dev_type as i32,
        });
        prog.push(Insn {
            code: BPF_JMP32_JNE_IMM,
            dst_reg: 3,
            src_reg: 0,
            off: jump_to(s + 7, next_rule),
            imm: rule.major as i32,
        });
        prog.push(Insn {
            code: BPF_JMP32_JNE_IMM,
            dst_reg: 4,
            src_reg: 0,
            off: jump_to(s + 8, next_rule),
            imm: rule.minor as i32,
        });
        prog.push(Insn {
            code: BPF_JA,
            dst_reg: 0,
            src_reg: 0,
            off: jump_to(s + 9, allow),
            imm: 0,
        });
    }

    debug_assert_eq!(prog.len(), allow);
    prog.push(mov_imm(0, 1)); // ALLOW
    prog.push(Insn {
        code: BPF_EXIT,
        dst_reg: 0,
        src_reg: 0,
        off: 0,
        imm: 0,
    });
    prog.push(mov_imm(0, 0)); // DENY
    prog.push(Insn {
        code: BPF_EXIT,
        dst_reg: 0,
        src_reg: 0,
        off: 0,
        imm: 0,
    });
    prog
}

/// Pack one raw instruction into the kernel's 64-bit encoding.
fn encode(insn: &Insn) -> u64 {
    (insn.code as u64)
        | ((insn.dst_reg & 0xf) as u64) << 8
        | ((insn.src_reg & 0xf) as u64) << 12
        | ((insn.off as u16 as u64) << 16)
        | ((insn.imm as u32 as u64) << 32)
}

/// Zero-padded kernel attribute block (union bpf_attr is 128 bytes and the
/// kernel requires unused tail bytes to be zero).
#[repr(C)]
struct AttrBlock([u64; 16]);

impl AttrBlock {
    fn zeroed() -> Self {
        AttrBlock([0u64; 16])
    }
    fn write_u32(&mut self, offset: usize, value: u32) {
        let word = &mut self.0[offset / 8];
        let shift = (offset % 8) * 8;
        *word &= !(0xffff_ffffu64 << shift);
        *word |= (value as u64) << shift;
    }
    fn write_u64(&mut self, offset: usize, value: u64) {
        self.0[offset / 8] = value;
    }
}

fn bpf_syscall(cmd: libc::c_int, attr: &AttrBlock) -> io::Result<i64> {
    // SAFETY: attr is a valid 128-byte block matching the kernel ABI for
    // this command; the syscall reads only the fields cmd defines.
    let ret = unsafe { libc::syscall(libc::SYS_bpf, cmd, attr, std::mem::size_of::<AttrBlock>()) };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn load_program(prog: &[Insn]) -> io::Result<i64> {
    let insns: Vec<u64> = prog.iter().map(encode).collect();
    let license = b"GPL\0";
    let mut attr = AttrBlock::zeroed();
    attr.write_u32(0, BPF_PROG_TYPE_CGROUP_DEVICE as u32); // prog_type
    attr.write_u32(4, prog.len() as u32); // insn_cnt
    attr.write_u64(8, insns.as_ptr() as usize as u64); // insns
    attr.write_u64(16, license.as_ptr() as usize as u64); // license
    attr.write_u32(68, BPF_CGROUP_DEVICE as u32); // expected_attach_type
    match bpf_syscall(BPF_PROG_LOAD, &attr) {
        Ok(fd) => Ok(fd),
        Err(_) => {
            // Retry with the verifier log so rejections carry diagnostics.
            let mut log = vec![0u8; 64 * 1024];
            attr.write_u32(24, 1); // log_level
            attr.write_u32(28, log.len() as u32); // log_size
            attr.write_u64(32, log.as_mut_ptr() as usize as u64); // log_buf
            let err = bpf_syscall(BPF_PROG_LOAD, &attr).unwrap_err();
            let kind = err.kind();
            let detail = String::from_utf8_lossy(&log);
            Err(io::Error::new(
                kind,
                format!(
                    "bpf(BPF_PROG_LOAD, cgroup_device): {err}: {}",
                    detail.trim_end_matches('\0').trim()
                ),
            ))
        }
    }
}

fn attach_to_cgroup(prog_fd: i64, cgroup_path: &Path) -> io::Result<()> {
    let target = std::fs::File::open(cgroup_path)
        .map_err(|err| io::Error::other(format!("open {}: {err}", cgroup_path.display())))?;
    let mut attr = AttrBlock::zeroed();
    attr.write_u32(0, target.as_raw_fd() as u32); // target_fd
    attr.write_u32(4, prog_fd as u32); // attach_bpf_fd
    attr.write_u32(8, BPF_CGROUP_DEVICE as u32); // attach_type
    bpf_syscall(BPF_PROG_ATTACH, &attr).map(|_| ())
}

fn device_numbers(path: &str) -> Option<(bool, u32, u32)> {
    let meta = std::fs::metadata(path).ok()?;
    let mode = meta.mode();
    let is_char = mode & libc::S_IFMT == libc::S_IFCHR;
    let is_block = mode & libc::S_IFMT == libc::S_IFBLK;
    if !is_char && !is_block {
        return None;
    }
    let dev = meta.rdev();
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfffu64);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xffu64);
    Some((is_char, major as u32, minor as u32))
}

/// Attach a deny-all-but-claimed device filter to `cgroup_path`. The
/// attachment holds a reference for the cgroup's lifetime, so the returned
/// loader fd can be dropped immediately.
pub fn enforce_devices(cgroup_path: &Path, devices: &[DeviceAccess]) -> Result<(), String> {
    let mut claimed: BTreeSet<(u32, u32, u32)> = BTreeSet::new();
    let mut rules = Vec::new();

    // Standard pseudo-devices every workload needs to function.
    for path in [
        "/dev/null",
        "/dev/zero",
        "/dev/full",
        "/dev/random",
        "/dev/urandom",
    ] {
        if let Some((is_char, major, minor)) = device_numbers(path) {
            let key = (
                if is_char {
                    DEVCG_DEV_CHAR
                } else {
                    DEVCG_DEV_BLOCK
                },
                major,
                minor,
            );
            if claimed.insert(key) {
                rules.push(DeviceRule {
                    dev_type: key.0,
                    major,
                    minor,
                    access: DEVCG_ACC_READ | DEVCG_ACC_WRITE,
                });
            }
        }
    }

    // Claimed devices get full access including mknod. Provider support
    // paths are part of the same logical claim (for example NVIDIA's
    // control and UVM devices), not independently schedulable resources.
    for device in devices {
        for path in std::iter::once(&device.dev).chain(device.paths.iter()) {
            let Some((is_char, major, minor)) = device_numbers(path) else {
                return Err(format!(
                    "device {} ({path}) is not reachable or not a device node",
                    device.id
                ));
            };
            let key = (
                if is_char {
                    DEVCG_DEV_CHAR
                } else {
                    DEVCG_DEV_BLOCK
                },
                major,
                minor,
            );
            if claimed.insert(key) {
                rules.push(DeviceRule {
                    dev_type: key.0,
                    major,
                    minor,
                    access: DEVCG_ACC_READ | DEVCG_ACC_WRITE | DEVCG_ACC_MKNOD,
                });
            }
        }
    }

    let prog = compile(&rules);
    let prog_fd = load_program(&prog).map_err(|err| err.to_string())?;
    attach_to_cgroup(prog_fd, cgroup_path).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal evaluator for exactly the instruction subset `compile`
    /// emits, so the deny-by-default semantics are testable without a
    /// kernel or CAP_BPF.
    fn run(prog: &[Insn], access_type: u32, major: u32, minor: u32) -> u64 {
        let mut regs = [0u32; 11];
        let ctx = [access_type, major, minor];
        let mut pc: i32 = 0;
        loop {
            let insn = &prog[pc as usize];
            match insn.code {
                0x61 => {
                    regs[insn.dst_reg as usize] = ctx[(insn.off / 4) as usize];
                }
                0xbf => {
                    regs[insn.dst_reg as usize] = regs[insn.src_reg as usize];
                }
                BPF_ALU32_MOV_IMM => {
                    regs[insn.dst_reg as usize] = insn.imm as u32;
                }
                BPF_ALU32_AND_IMM => {
                    regs[insn.dst_reg as usize] &= insn.imm as u32;
                }
                BPF_ALU32_RSH_IMM => {
                    regs[insn.dst_reg as usize] >>= insn.imm;
                }
                BPF_JMP32_JNE_IMM => {
                    if regs[insn.dst_reg as usize] != insn.imm as u32 {
                        pc += insn.off as i32 + 1;
                    } else {
                        pc += 1;
                    }
                    continue;
                }
                BPF_JA => {
                    pc += insn.off as i32;
                }
                BPF_EXIT => return regs[0] as u64,
                other => panic!("interpreter does not implement {other:#x}"),
            }
            pc += 1;
        }
    }

    fn rule(dev_type: u32, major: u32, minor: u32, access: u32) -> DeviceRule {
        DeviceRule {
            dev_type,
            major,
            minor,
            access,
        }
    }

    fn ctx_of(flags: u32, dev_type: u32, major: u32, minor: u32) -> (u32, u32, u32) {
        ((dev_type << 16) | flags, major, minor)
    }

    #[test]
    fn empty_rules_deny_everything() {
        let prog = compile(&[]);
        for ctx in [
            ctx_of(DEVCG_ACC_READ, DEVCG_DEV_CHAR, 1, 3),    // null
            ctx_of(DEVCG_ACC_WRITE, DEVCG_DEV_CHAR, 195, 0), // nvidia
            ctx_of(DEVCG_ACC_MKNOD, DEVCG_DEV_BLOCK, 8, 0),
        ] {
            assert_eq!(run(&prog, ctx.0, ctx.1, ctx.2), 0);
        }
    }

    #[test]
    fn claimed_device_allowed_others_denied() {
        let prog = compile(&[
            rule(DEVCG_DEV_CHAR, 195, 0, DEVCG_ACC_READ | DEVCG_ACC_WRITE),
            rule(DEVCG_DEV_BLOCK, 8, 16, DEVCG_ACC_MKNOD),
        ]);
        // Claimed GPU: read and write allowed.
        assert_eq!(
            run(
                &prog,
                ctx_of(DEVCG_ACC_READ, DEVCG_DEV_CHAR, 195, 0).0,
                195,
                0
            ),
            1
        );
        assert_eq!(
            run(
                &prog,
                ctx_of(DEVCG_ACC_WRITE, DEVCG_DEV_CHAR, 195, 0).0,
                195,
                0
            ),
            1
        );
        // mknod on the GPU is not granted.
        assert_eq!(
            run(
                &prog,
                ctx_of(DEVCG_ACC_MKNOD, DEVCG_DEV_CHAR, 195, 0).0,
                195,
                0
            ),
            0
        );
        // Claimed block device grants only mknod per its rule.
        assert_eq!(
            run(
                &prog,
                ctx_of(DEVCG_ACC_MKNOD, DEVCG_DEV_BLOCK, 8, 16).0,
                8,
                16
            ),
            1
        );
        assert_eq!(
            run(
                &prog,
                ctx_of(DEVCG_ACC_READ, DEVCG_DEV_BLOCK, 8, 16).0,
                8,
                16
            ),
            0
        );
        // A different minor of the same controller is denied.
        assert_eq!(
            run(
                &prog,
                ctx_of(DEVCG_ACC_READ, DEVCG_DEV_CHAR, 195, 1).0,
                195,
                1
            ),
            0
        );
        // Wrong device type with matching numbers is denied.
        assert_eq!(
            run(
                &prog,
                ctx_of(DEVCG_ACC_READ, DEVCG_DEV_BLOCK, 195, 0).0,
                195,
                0
            ),
            0
        );
    }

    #[test]
    fn first_matching_rule_wins_and_later_rules_still_apply() {
        let prog = compile(&[
            rule(DEVCG_DEV_CHAR, 5, 1, DEVCG_ACC_READ),
            rule(DEVCG_DEV_CHAR, 5, 2, DEVCG_ACC_WRITE),
        ]);
        assert_eq!(
            run(&prog, ctx_of(DEVCG_ACC_READ, DEVCG_DEV_CHAR, 5, 1).0, 5, 1),
            1
        );
        assert_eq!(
            run(&prog, ctx_of(DEVCG_ACC_WRITE, DEVCG_DEV_CHAR, 5, 2).0, 5, 2),
            1
        );
    }

    #[test]
    fn encoding_matches_kernel_layout() {
        let prog = compile(&[]);
        let packed = encode(&prog[0]);
        assert_eq!(packed & 0xff, prog[0].code as u64);
        assert_eq!((packed >> 8) & 0xf, prog[0].dst_reg as u64);
        assert_eq!((packed >> 12) & 0xf, prog[0].src_reg as u64);
        assert_eq!((packed >> 16) & 0xffff, prog[0].off as u16 as u64);
        assert_eq!(packed >> 32, prog[0].imm as u32 as u64);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod live_tests {
    use super::*;

    #[test]
    fn loads_minimal_program_when_privileged() {
        let fd = load_program(&compile(&[]));
        match fd {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                eprintln!("skipping: bpf() requires privileges here");
            }
            Err(err) => panic!("minimal deny program failed to load: {err}"),
        }
    }
}
