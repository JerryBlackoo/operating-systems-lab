use alloc::sync::Arc;
use alloc::vec::Vec;

use super::socket::{create_socket_file, SocketError, SocketFile};

const AF_ALG: i32 = 38;
const SOCK_SEQPACKET: i32 = 5;
pub(in crate::net) const SOL_ALG: i32 = 279;
const ALG_SET_KEY: i32 = 1;
const ALG_SET_IV: i32 = 2;
const ALG_SET_OP: i32 = 3;
const ALG_SET_AEAD_ASSOCLEN: i32 = 4;
const ALG_OP_DECRYPT: u32 = 0;
const ALG_OP_ENCRYPT: u32 = 1;
const MSG_MORE: i32 = 0x8000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::net) enum AfAlgRole {
    Control,
    Request,
}

#[derive(Clone)]
pub(in crate::net) struct AfAlgState {
    pub(in crate::net) role: AfAlgRole,
    alg_type: Vec<u8>,
    alg_name: Vec<u8>,
    key: Vec<u8>,
    input: Vec<u8>,
    input_len: usize,
    op: u32,
    assoclen: u32,
    operation_ready: bool,
    read_done: bool,
}

impl AfAlgState {
    pub(in crate::net) fn new() -> Self {
        Self {
            role: AfAlgRole::Control,
            alg_type: Vec::new(),
            alg_name: Vec::new(),
            key: Vec::new(),
            input: Vec::new(),
            input_len: 0,
            op: ALG_OP_DECRYPT,
            assoclen: 0,
            operation_ready: false,
            read_done: false,
        }
    }
}

pub fn bind(socket: Arc<SocketFile>, addr: &[u8], addrlen: usize) -> Result<(), SocketError> {
    if addrlen < 88 || addr.len() < 88 {
        return Err(SocketError::InvalidArg);
    }
    let family = u16::from_ne_bytes([addr[0], addr[1]]) as i32;
    if family != AF_ALG {
        return Err(SocketError::AddrFamilyNotSupported);
    }
    let alg_type = nul_field(&addr[2..16]);
    let alg_name = nul_field(&addr[24..88]);
    crate::dprintln!("[af_alg] bind addrlen={} type={:?} name={:?}", addrlen, alg_type, alg_name);
    if alg_type.is_empty() || alg_name.is_empty() {
        return Err(SocketError::InvalidArg);
    }
    validate_algorithm(&alg_type, &alg_name)?;
    socket.with_state_mut(|state| {
        if state.sock_type != SOCK_SEQPACKET || state.proto != 0 || state.bound {
            return Err(SocketError::InvalidArg);
        }
        let af = state.backend.af_alg_mut();
        if af.role != AfAlgRole::Control {
            return Err(SocketError::InvalidArg);
        }
        af.alg_type = alg_type;
        af.alg_name = alg_name;
        af.key.clear();
        af.input.clear();
        af.input_len = 0;
        af.operation_ready = false;
        af.read_done = false;
        state.bound = true;
        state.bound_addr[0..2].copy_from_slice(&(AF_ALG as u16).to_ne_bytes());
        Ok(())
    })
}

pub(in crate::net) fn setsockopt(state: &mut AfAlgState, optname: i32, value: &[u8]) -> Result<(), SocketError> {
    crate::dprintln!("[af_alg] setsockopt role={:?} opt={} len={} type={:?} name={:?}", state.role, optname, value.len(), state.alg_type, state.alg_name);
    match optname {
        ALG_SET_KEY => set_key(state, value),
        _ => Err(SocketError::NoProtocolOption),
    }
}

pub fn accept(listener: Arc<SocketFile>) -> Result<(Arc<SocketFile>, [u8; 16]), SocketError> {
    crate::dprintln!("[af_alg] accept enter");
    let snapshot = listener.with_state(|state| {
        if state.closed || !state.bound || state.sock_type != SOCK_SEQPACKET {
            return Err(SocketError::InvalidArg);
        }
        let af = state.backend.af_alg();
        if af.role != AfAlgRole::Control {
            return Err(SocketError::InvalidArg);
        }
        Ok(af.clone())
    })?;
    let req = create_socket_file(AF_ALG, SOCK_SEQPACKET, 0, false);
    crate::dprintln!("[af_alg] accept ok type={:?} name={:?}", snapshot.alg_type, snapshot.alg_name);
    req.with_state_mut(|state| {
        let af = state.backend.af_alg_mut();
        *af = snapshot;
        af.role = AfAlgRole::Request;
        af.input.clear();
        af.input_len = 0;
        af.operation_ready = false;
        af.read_done = false;
        state.bound = true;
        state.bound_addr[0..2].copy_from_slice(&(AF_ALG as u16).to_ne_bytes());
        state.peer_pid = Some(listener.with_state(|listener_state| listener_state.owner_pid));
    });
    Ok((req, [0u8; 16]))
}

pub fn send(socket: Arc<SocketFile>, data: &[u8], flags: i32, control: Option<&[u8]>) -> Result<usize, SocketError> {
    socket.with_state_mut(|state| {
        if state.closed {
            return Err(SocketError::BrokenPipe);
        }
        let af = state.backend.af_alg_mut();
        crate::dprintln!("[af_alg] send role={:?} flags={:#x} len={} ctl_len={} ready={} input_len={} type={:?} name={:?}", af.role, flags, data.len(), control.map(|c| c.len()).unwrap_or(0), af.operation_ready, af.input_len, af.alg_type, af.alg_name);
        if af.role != AfAlgRole::Request {
            return Err(SocketError::OpNotSupported);
        }
        if let Some(control) = control {
            parse_control(af, control)?;
        }
        if is_hash(&af.alg_type) {
            af.input_len = af.input_len.saturating_add(data.len());
        } else {
            if !data.is_empty() {
                af.input.extend_from_slice(data);
            }
            af.input_len = af.input_len.saturating_add(data.len());
        }
        af.operation_ready = flags & MSG_MORE == 0;
        af.read_done = false;
        crate::dprintln!("[af_alg] send ok ready={} input_len={} read_done={}", af.operation_ready, af.input_len, af.read_done);
        Ok(data.len())
    })
}

pub fn recv(socket: Arc<SocketFile>) -> Result<(Vec<u8>, [u8; 16]), SocketError> {
    socket.with_state_mut(|state| {
        if state.closed {
            return Err(SocketError::BrokenPipe);
        }
        let af = state.backend.af_alg_mut();
        crate::dprintln!("[af_alg] recv role={:?} ready={} read_done={} input_len={} type={:?} name={:?}", af.role, af.operation_ready, af.read_done, af.input_len, af.alg_type, af.alg_name);
        if af.role != AfAlgRole::Request {
            return Err(SocketError::OpNotSupported);
        }
        if !af.operation_ready && !is_hash(&af.alg_type) {
            crate::dprintln!("[af_alg] recv wouldblock ready={} input_len={} type={:?} name={:?}", af.operation_ready, af.input_len, af.alg_type, af.alg_name);
            return Err(SocketError::WouldBlock);
        }
        let out = if is_hash(&af.alg_type) {
            let len = digest_len(&af.alg_name);
            af.read_done = true;
            alloc::vec![0u8; len]
        } else if is_skcipher(&af.alg_type) {
            if af.read_done {
                Vec::new()
            } else {
                let block = skcipher_block_size(&af.alg_name);
                if block > 1 && af.input_len % block != 0 {
                    return Err(SocketError::InvalidArg);
                }
                af.read_done = true;
                core::mem::take(&mut af.input)
            }
        } else if is_aead(&af.alg_type) {
            if af.read_done {
                Vec::new()
            } else {
                af.read_done = true;
                core::mem::take(&mut af.input)
            }
        } else {
            return Err(SocketError::InvalidArg);
        };
        crate::dprintln!("[af_alg] recv ok len={} read_done={} input_len={}", out.len(), af.read_done, af.input_len);
        Ok((out, [0u8; 16]))
    })
}

pub fn read_ready(socket: &SocketFile) -> bool {
    socket.with_state(|state| {
        let af = state.backend.af_alg();
        af.role == AfAlgRole::Request && (af.operation_ready || is_hash(&af.alg_type) || af.read_done)
    })
}

fn set_key(state: &mut AfAlgState, value: &[u8]) -> Result<(), SocketError> {
    if state.role != AfAlgRole::Control || state.alg_type.is_empty() {
        return Err(SocketError::InvalidArg);
    }
    if is_aead(&state.alg_type) && eq(&state.alg_name, b"authenc(hmac(sha256),cbc(aes))") {
        if value.len() < 12 || value.len() % 4 != 0 {
            return Err(SocketError::InvalidArg);
        }
    }
    if is_skcipher(&state.alg_type) && value.is_empty() {
        return Err(SocketError::InvalidArg);
    }
    if is_hash(&state.alg_type) && is_vmac(&state.alg_name) && value.len() != 16 {
        return Err(SocketError::InvalidArg);
    }
    state.key.clear();
    state.key.extend_from_slice(value);
    Ok(())
}

fn parse_control(state: &mut AfAlgState, control: &[u8]) -> Result<(), SocketError> {
    crate::dprintln!("[af_alg] parse_control len={}", control.len());
    let mut offset = 0usize;
    while offset + 16 <= control.len() {
        let cmsg_len = usize::from_ne_bytes(control[offset..offset + 8].try_into().unwrap());
        if cmsg_len < 16 || offset + cmsg_len > control.len() {
            return Err(SocketError::InvalidArg);
        }
        let level = i32::from_ne_bytes(control[offset + 8..offset + 12].try_into().unwrap());
        let ty = i32::from_ne_bytes(control[offset + 12..offset + 16].try_into().unwrap());
        let data = &control[offset + 16..offset + cmsg_len];
        if level == SOL_ALG {
            crate::dprintln!("[af_alg] cmsg level={} ty={} data_len={}", level, ty, data.len());
            match ty {
                ALG_SET_OP => {
                    if data.len() < 4 {
                        return Err(SocketError::InvalidArg);
                    }
                    let op = u32::from_ne_bytes(data[..4].try_into().unwrap());
                    if op != ALG_OP_DECRYPT && op != ALG_OP_ENCRYPT {
                        return Err(SocketError::InvalidArg);
                    }
                    state.op = op;
                }
                ALG_SET_IV => {
                    if data.len() < 4 {
                        return Err(SocketError::InvalidArg);
                    }
                    let ivlen = u32::from_ne_bytes(data[..4].try_into().unwrap()) as usize;
                    if data.len() < 4 + ivlen {
                        return Err(SocketError::InvalidArg);
                    }
                }
                ALG_SET_AEAD_ASSOCLEN => {
                    if data.len() < 4 {
                        return Err(SocketError::InvalidArg);
                    }
                    state.assoclen = u32::from_ne_bytes(data[..4].try_into().unwrap());
                }
                _ => return Err(SocketError::NoProtocolOption),
            }
        }
        offset = align_usize(offset + cmsg_len);
    }
    if offset != control.len() && control[offset..].iter().any(|&b| b != 0) {
        return Err(SocketError::InvalidArg);
    }
    Ok(())
}

fn validate_algorithm(alg_type: &[u8], alg_name: &[u8]) -> Result<(), SocketError> {
    if is_hash(alg_type) {
        if supported_hash(alg_name) {
            Ok(())
        } else {
            Err(SocketError::NoEntry)
        }
    } else if is_skcipher(alg_type) {
        if eq(alg_name, b"salsa20") || eq(alg_name, b"cbc(aes-generic)") {
            Ok(())
        } else {
            Err(SocketError::NoEntry)
        }
    } else if is_aead(alg_type) {
        if eq(alg_name, b"rfc7539(chacha20,poly1305)") || eq(alg_name, b"authenc(hmac(sha256),cbc(aes))") {
            Ok(())
        } else {
            Err(SocketError::NoEntry)
        }
    } else {
        Err(SocketError::NoEntry)
    }
}

fn supported_hash(name: &[u8]) -> bool {
    base_hash(name) || supported_hmac(name) || is_vmac(name)
}

fn base_hash(name: &[u8]) -> bool {
    eq(name, b"md5") || eq(name, b"md5-generic")
        || eq(name, b"sha1") || eq(name, b"sha1-generic")
        || eq(name, b"sha224") || eq(name, b"sha224-generic")
        || eq(name, b"sha256") || eq(name, b"sha256-generic")
        || eq(name, b"sha3-256") || eq(name, b"sha3-256-generic")
        || eq(name, b"sha3-512") || eq(name, b"sha3-512-generic")
        || eq(name, b"sm3") || eq(name, b"sm3-generic")
}

fn supported_hmac(name: &[u8]) -> bool {
    let Some(inner) = template_inner(name, b"hmac(") else {
        return false;
    };
    base_hash(inner)
}

fn is_vmac(name: &[u8]) -> bool {
    eq(name, b"vmac64(aes)") || eq(name, b"vmac(aes)")
}

fn digest_len(name: &[u8]) -> usize {
    if let Some(inner) = template_inner(name, b"hmac(") {
        return digest_len(inner);
    }
    if name.starts_with(b"sha3-512") { 64 }
    else if name.starts_with(b"sha3-256") || name.starts_with(b"sha256") || name.starts_with(b"sm3") { 32 }
    else if name.starts_with(b"sha224") { 28 }
    else if name.starts_with(b"sha1") { 20 }
    else if name.starts_with(b"md5") { 16 }
    else if is_vmac(name) { 8 }
    else { 32 }
}

fn skcipher_block_size(name: &[u8]) -> usize {
    if eq(name, b"cbc(aes-generic)") { 16 } else { 1 }
}

fn template_inner<'a>(name: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if name.starts_with(prefix) && name.ends_with(b")") && name.len() > prefix.len() + 1 {
        Some(&name[prefix.len()..name.len() - 1])
    } else {
        None
    }
}

fn nul_field(field: &[u8]) -> Vec<u8> {
    let len = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    field[..len].to_vec()
}

fn is_hash(alg_type: &[u8]) -> bool { eq(alg_type, b"hash") }
fn is_skcipher(alg_type: &[u8]) -> bool { eq(alg_type, b"skcipher") }
fn is_aead(alg_type: &[u8]) -> bool { eq(alg_type, b"aead") }
fn eq(left: &[u8], right: &[u8]) -> bool { left == right }

fn align_usize(value: usize) -> usize {
    let align = core::mem::size_of::<usize>();
    (value + align - 1) & !(align - 1)
}
