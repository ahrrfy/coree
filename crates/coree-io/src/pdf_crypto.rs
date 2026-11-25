// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust `#![forbid(unsafe_code)]` PDF Standard Security Handler decryption
//! (RC4 40/128-bit `R=2..4`, AES-128-CBC `R=4`, and AES-256-CBC `R=5..6`).

use crate::error::{IoError, IoResult};
use std::collections::BTreeMap;

/// PDF Standard padding string (32 bytes, ISO 32000-1 §7.6.3.3).
const PDF_PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PdfCipher {
    None,
    Rc4,
    Aes128Cbc,
    Aes256Cbc,
}

#[derive(Debug, Clone)]
pub(crate) struct PdfCryptoCtx {
    file_key: Vec<u8>,
    str_cipher: PdfCipher,
    stm_cipher: PdfCipher,
    encrypt_metadata: bool,
    pub(crate) encrypt_obj_num: Option<u32>,
}

/// Extracted parameters from the `/Encrypt` dictionary needed for key derivation.
pub(crate) struct EncryptDictParams<'a> {
    pub filter: &'a str,
    pub v: i64,
    pub r: i64,
    pub length_bits: usize,
    pub p: i32,
    pub o: &'a [u8],
    pub u: &'a [u8],
    pub oe: &'a [u8],
    pub ue: &'a [u8],
    pub id0: &'a [u8],
    pub encrypt_metadata: bool,
    pub str_cipher: PdfCipher,
    pub stm_cipher: PdfCipher,
    pub encrypt_obj_num: Option<u32>,
}

impl PdfCryptoCtx {
    pub(crate) fn from_params(params: &EncryptDictParams<'_>, password: &str) -> IoResult<Self> {
        if !params.filter.is_empty() && params.filter != "Standard" {
            return Err(IoError::InvalidFormat {
                format: "pdf".into(),
                message: format!("unsupported PDF /Encrypt filter '{}'", params.filter),
            });
        }
        let file_key = if params.v == 5 || params.r >= 5 {
            derive_file_key_r5_r6(params, password)?
        } else {
            derive_file_key_r2_r4(params, password)?
        };
        Ok(Self {
            file_key,
            str_cipher: params.str_cipher,
            stm_cipher: params.stm_cipher,
            encrypt_metadata: params.encrypt_metadata,
            encrypt_obj_num: params.encrypt_obj_num,
        })
    }

    pub(crate) fn decrypt_string(&self, raw: &[u8], obj_num: u32, gen_num: u16) -> Vec<u8> {
        if self.encrypt_obj_num == Some(obj_num) {
            return raw.to_vec();
        }
        self.decrypt_with_cipher(self.str_cipher, raw, obj_num, gen_num)
    }

    pub(crate) fn decrypt_stream(
        &self,
        raw: &[u8],
        obj_num: u32,
        gen_num: u16,
        stream_type: Option<&str>,
        filters: &[String],
        crypt_filter_name: Option<&str>,
    ) -> Vec<u8> {
        if self.encrypt_obj_num == Some(obj_num) {
            return raw.to_vec();
        }
        if !self.encrypt_metadata && stream_type == Some("Metadata") {
            return raw.to_vec();
        }
        if filters.iter().any(|f| f == "Crypt") && crypt_filter_name == Some("Identity") {
            return raw.to_vec();
        }
        self.decrypt_with_cipher(self.stm_cipher, raw, obj_num, gen_num)
    }

    fn decrypt_with_cipher(
        &self,
        cipher: PdfCipher,
        raw: &[u8],
        obj_num: u32,
        gen_num: u16,
    ) -> Vec<u8> {
        match cipher {
            PdfCipher::None => raw.to_vec(),
            PdfCipher::Rc4 => {
                let obj_key = derive_per_object_key(&self.file_key, obj_num, gen_num, false);
                rc4_crypt(&obj_key, raw)
            }
            PdfCipher::Aes128Cbc => {
                if raw.len() < 16 {
                    return Vec::new();
                }
                let obj_key = derive_per_object_key(&self.file_key, obj_num, gen_num, true);
                let mut key16 = [0u8; 16];
                let copy_len = obj_key.len().min(16);
                key16[..copy_len].copy_from_slice(&obj_key[..copy_len]);
                aes_128_cbc_decrypt(&key16, &raw[..16], &raw[16..])
            }
            PdfCipher::Aes256Cbc => {
                if raw.len() < 16 || self.file_key.len() < 32 {
                    return Vec::new();
                }
                let mut key32 = [0u8; 32];
                key32.copy_from_slice(&self.file_key[..32]);
                aes_256_cbc_decrypt(&key32, &raw[..16], &raw[16..])
            }
        }
    }
}

pub(crate) fn parse_cipher_map(
    cf_cfms: &BTreeMap<String, PdfCipher>,
    strf: Option<&str>,
    stmf: Option<&str>,
    v: i64,
) -> (PdfCipher, PdfCipher) {
    if v < 4 {
        return (PdfCipher::Rc4, PdfCipher::Rc4);
    }
    let resolve_cf = |name: Option<&str>| -> PdfCipher {
        match name.unwrap_or("StdCF") {
            "Identity" => PdfCipher::None,
            n => cf_cfms.get(n).copied().unwrap_or(PdfCipher::Rc4),
        }
    };
    (resolve_cf(strf), resolve_cf(stmf))
}

fn derive_per_object_key(file_key: &[u8], obj_num: u32, gen_num: u16, is_aes: bool) -> Vec<u8> {
    let mut buf = Vec::with_capacity(file_key.len() + 9);
    buf.extend_from_slice(file_key);
    let obj_le = obj_num.to_le_bytes();
    let gen_le = gen_num.to_le_bytes();
    buf.extend_from_slice(&obj_le[..3]);
    buf.extend_from_slice(&gen_le[..2]);
    if is_aes {
        buf.extend_from_slice(b"sAlT");
    }
    let digest = md5(&buf);
    let out_len = (file_key.len() + 5).min(16);
    digest[..out_len].to_vec()
}

fn pad_pdf_password(pw: &str) -> [u8; 32] {
    let mut out = PDF_PADDING;
    let bytes = pw.as_bytes();
    let n = bytes.len().min(32);
    out[..n].copy_from_slice(&bytes[..n]);
    if n < 32 {
        out[n..].copy_from_slice(&PDF_PADDING[..(32 - n)]);
    }
    out
}

fn derive_file_key_r2_r4(params: &EncryptDictParams<'_>, password: &str) -> IoResult<Vec<u8>> {
    let key_len = if params.r == 2 {
        5
    } else {
        (params.length_bits / 8).clamp(5, 16)
    };
    let padded = pad_pdf_password(password);

    let compute_for_padded = |pwd32: &[u8; 32]| -> Vec<u8> {
        let mut buf = Vec::with_capacity(32 + params.o.len() + 4 + params.id0.len() + 4);
        buf.extend_from_slice(pwd32);
        buf.extend_from_slice(params.o);
        buf.extend_from_slice(&params.p.to_le_bytes());
        buf.extend_from_slice(params.id0);
        if params.r >= 4 && !params.encrypt_metadata {
            buf.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        }
        let mut digest = md5(&buf);
        if params.r >= 3 {
            for _ in 0..50 {
                digest = md5(&digest[..key_len]);
            }
        }
        digest[..key_len].to_vec()
    };

    let check_user_key = |fkey: &[u8]| -> bool {
        if params.r == 2 {
            let expected = rc4_crypt(fkey, &PDF_PADDING);
            params.u.len() >= 32 && params.u[..32] == expected[..]
        } else {
            let mut h_buf = Vec::with_capacity(32 + params.id0.len());
            h_buf.extend_from_slice(&PDF_PADDING);
            h_buf.extend_from_slice(params.id0);
            let h = md5(&h_buf);
            let mut u_calc = rc4_crypt(fkey, &h);
            for i in 1u8..=19 {
                let mut iter_key = fkey.to_vec();
                for b in &mut iter_key {
                    *b ^= i;
                }
                u_calc = rc4_crypt(&iter_key, &u_calc);
            }
            params.u.len() >= 16 && params.u[..16] == u_calc[..16]
        }
    };

    // 1. Try as user password directly
    let candidate = compute_for_padded(&padded);
    if check_user_key(&candidate) {
        return Ok(candidate);
    }

    // 2. Try as owner password (Algorithm 7 -> recovers user password)
    if params.o.len() >= 32 {
        let mut owner_hash = md5(&padded);
        if params.r >= 3 {
            for _ in 0..50 {
                owner_hash = md5(&owner_hash);
            }
        }
        let ok = &owner_hash[..key_len];
        let mut user_padded_vec = if params.r == 2 {
            rc4_crypt(ok, &params.o[..32])
        } else {
            let mut tmp = params.o[..32].to_vec();
            for i in (0u8..=19).rev() {
                let mut iter_key = ok.to_vec();
                for b in &mut iter_key {
                    *b ^= i;
                }
                tmp = rc4_crypt(&iter_key, &tmp);
            }
            tmp
        };
        user_padded_vec.resize(32, 0);
        let mut user_padded = [0u8; 32];
        user_padded.copy_from_slice(&user_padded_vec[..32]);
        let owner_candidate = compute_for_padded(&user_padded);
        if check_user_key(&owner_candidate) {
            return Ok(owner_candidate);
        }
    }

    Err(IoError::InvalidFormat {
        format: "pdf".into(),
        message: "encrypted PDF requires a valid password (wrong or missing password)".into(),
    })
}

fn derive_file_key_r5_r6(params: &EncryptDictParams<'_>, password: &str) -> IoResult<Vec<u8>> {
    let pw_bytes = password.as_bytes();
    let pw_trunc = &pw_bytes[..pw_bytes.len().min(127)];
    if params.u.len() < 48 || params.ue.len() < 32 {
        return Err(IoError::InvalidFormat {
            format: "pdf".into(),
            message: "malformed R=5/6 /Encrypt dictionary (/U or /UE too short)".into(),
        });
    }

    let hash_fn = |pw: &[u8], salt: &[u8], udata: &[u8]| -> [u8; 32] {
        if params.r == 5 {
            let mut buf = Vec::with_capacity(pw.len() + salt.len() + udata.len());
            buf.extend_from_slice(pw);
            buf.extend_from_slice(salt);
            buf.extend_from_slice(udata);
            sha256(&buf)
        } else {
            algorithm_2b_r6_hash(pw, salt, udata)
        }
    };

    // 1. User password check (Algorithm 2.A / 11)
    let u_val_salt = &params.u[32..40];
    let u_key_salt = &params.u[40..48];
    let u_hash = hash_fn(pw_trunc, u_val_salt, &[]);
    if u_hash[..] == params.u[..32] {
        let wrap_key = hash_fn(pw_trunc, u_key_salt, &[]);
        let file_key = aes_256_cbc_decrypt_no_pad(&wrap_key, &[0u8; 16], &params.ue[..32]);
        if file_key.len() == 32 {
            return Ok(file_key);
        }
    }

    // 2. Owner password check
    if params.o.len() >= 48 && params.oe.len() >= 32 {
        let o_val_salt = &params.o[32..40];
        let o_key_salt = &params.o[40..48];
        let o_hash = hash_fn(pw_trunc, o_val_salt, &params.u[..48]);
        if o_hash[..] == params.o[..32] {
            let wrap_key = hash_fn(pw_trunc, o_key_salt, &params.u[..48]);
            let file_key = aes_256_cbc_decrypt_no_pad(&wrap_key, &[0u8; 16], &params.oe[..32]);
            if file_key.len() == 32 {
                return Ok(file_key);
            }
        }
    }

    Err(IoError::InvalidFormat {
        format: "pdf".into(),
        message: "encrypted PDF (AES-256) requires a valid password".into(),
    })
}

/// PDF 2.0 Algorithm 2.B key-hardening iterative hash (`R=6`).
fn algorithm_2b_r6_hash(password: &[u8], salt: &[u8], udata: &[u8]) -> [u8; 32] {
    let mut init = Vec::with_capacity(password.len() + salt.len() + udata.len());
    init.extend_from_slice(password);
    init.extend_from_slice(salt);
    init.extend_from_slice(udata);
    let mut k = sha256(&init).to_vec();

    let mut round = 0usize;
    loop {
        let unit_len = password.len() + k.len() + udata.len();
        let mut k1 = Vec::with_capacity(unit_len * 64);
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }
        let mut aes_key = [0u8; 16];
        let mut aes_iv = [0u8; 16];
        aes_key.copy_from_slice(&k[..16]);
        aes_iv.copy_from_slice(&k[16..32]);
        let e = aes_128_cbc_encrypt_no_pad(&aes_key, &aes_iv, &k1);
        let sum_mod3 = e[..16].iter().fold(0u32, |acc, &b| acc + u32::from(b)) % 3;
        k = match sum_mod3 {
            0 => sha256(&e).to_vec(),
            1 => sha384(&e).to_vec(),
            _ => sha512(&e).to_vec(),
        };
        round += 1;
        let last_byte = *e.last().unwrap_or(&0) as usize;
        if round >= 64 && last_byte <= round - 32 {
            break;
        }
        if round > 1024 {
            break;
        }
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&k[..32]);
    out
}

pub(crate) fn rc4_crypt(key: &[u8], data: &[u8]) -> Vec<u8> {
    if key.is_empty() {
        return data.to_vec();
    }
    let mut s = [0u8; 256];
    for (i, slot) in s.iter_mut().enumerate() {
        *slot = i as u8;
    }
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, usize::from(j));
    }
    let mut i = 0u8;
    j = 0u8;
    let mut out = Vec::with_capacity(data.len());
    for &b in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[usize::from(i)]);
        s.swap(usize::from(i), usize::from(j));
        let k = s[usize::from(s[usize::from(i)].wrapping_add(s[usize::from(j)]))];
        out.push(b ^ k);
    }
    out
}

// =============================================================================
// Pure-Rust AES-128 & AES-256 (CBC)
// =============================================================================

#[rustfmt::skip]
const AES_SBOX: [u8; 256] = [
    0x63,0x7c,0x77,0x7b,0xf2,0x6b,0x6f,0xc5,0x30,0x01,0x67,0x2b,0xfe,0xd7,0xab,0x76,
    0xca,0x82,0xc9,0x7d,0xfa,0x59,0x47,0xf0,0xad,0xd4,0xa2,0xaf,0x9c,0xa4,0x72,0xc0,
    0xb7,0xfd,0x93,0x26,0x36,0x3f,0xf7,0xcc,0x34,0xa5,0xe5,0xf1,0x71,0xd8,0x31,0x15,
    0x04,0xc7,0x23,0xc3,0x18,0x96,0x05,0x9a,0x07,0x12,0x80,0xe2,0xeb,0x27,0xb2,0x75,
    0x09,0x83,0x2c,0x1a,0x1b,0x6e,0x5a,0xa0,0x52,0x3b,0xd6,0xb3,0x29,0xe3,0x2f,0x84,
    0x53,0xd1,0x00,0xed,0x20,0xfc,0xb1,0x5b,0x6a,0xcb,0xbe,0x39,0x4a,0x4c,0x58,0xcf,
    0xd0,0xef,0xaa,0xfb,0x43,0x4d,0x33,0x85,0x45,0xf9,0x02,0x7f,0x50,0x3c,0x9f,0xa8,
    0x51,0xa3,0x40,0x8f,0x92,0x9d,0x38,0xf5,0xbc,0xb6,0xda,0x21,0x10,0xff,0xf3,0xd2,
    0xcd,0x0c,0x13,0xec,0x5f,0x97,0x44,0x17,0xc4,0xa7,0x7e,0x3d,0x64,0x5d,0x19,0x73,
    0x60,0x81,0x4f,0xdc,0x22,0x2a,0x90,0x88,0x46,0xee,0xb8,0x14,0xde,0x5e,0x0b,0xdb,
    0xe0,0x32,0x3a,0x0a,0x49,0x06,0x24,0x5c,0xc2,0xd3,0xac,0x62,0x91,0x95,0xe4,0x79,
    0xe7,0xc8,0x37,0x6d,0x8d,0xd5,0x4e,0xa9,0x6c,0x56,0xf4,0xea,0x65,0x7a,0xae,0x08,
    0xba,0x78,0x25,0x2e,0x1c,0xa6,0xb4,0xc6,0xe8,0xdd,0x74,0x1f,0x4b,0xbd,0x8b,0x8a,
    0x70,0x3e,0xb5,0x66,0x48,0x03,0xf6,0x0e,0x61,0x35,0x57,0xb9,0x86,0xc1,0x1d,0x9e,
    0xe1,0xf8,0x98,0x11,0x69,0xd9,0x8e,0x94,0x9b,0x1e,0x87,0xe9,0xce,0x55,0x28,0xdf,
    0x8c,0xa1,0x89,0x0d,0xbf,0xe6,0x42,0x68,0x41,0x99,0x2d,0x0f,0xb0,0x54,0xbb,0x16,
];

#[rustfmt::skip]
const AES_INV_SBOX: [u8; 256] = [
    0x52,0x09,0x6a,0xd5,0x30,0x36,0xa5,0x38,0xbf,0x40,0xa3,0x9e,0x81,0xf3,0xd7,0xfb,
    0x7c,0xe3,0x39,0x82,0x9b,0x2f,0xff,0x87,0x34,0x8e,0x43,0x44,0xc4,0xde,0xe9,0xcb,
    0x54,0x7b,0x94,0x32,0xa6,0xc2,0x23,0x3d,0xee,0x4c,0x95,0x0b,0x42,0xfa,0xc3,0x4e,
    0x08,0x2e,0xa1,0x66,0x28,0xd9,0x24,0xb2,0x76,0x5b,0xa2,0x49,0x6d,0x8b,0xd1,0x25,
    0x72,0xf8,0xf6,0x64,0x86,0x68,0x98,0x16,0xd4,0xa4,0x5c,0xcc,0x5d,0x65,0xb6,0x92,
    0x6c,0x70,0x48,0x50,0xfd,0xed,0xb9,0xda,0x5e,0x15,0x46,0x57,0xa7,0x8d,0x9d,0x84,
    0x90,0xd8,0xab,0x00,0x8c,0xbc,0xd3,0x0a,0xf7,0xe4,0x58,0x05,0xb8,0xb3,0x45,0x06,
    0xd0,0x2c,0x1e,0x8f,0xca,0x3f,0x0f,0x02,0xc1,0xaf,0xbd,0x03,0x01,0x13,0x8a,0x6b,
    0x3a,0x91,0x11,0x41,0x4f,0x67,0xdc,0xea,0x97,0xf2,0xcf,0xce,0xf0,0xb4,0xe6,0x73,
    0x96,0xac,0x74,0x22,0xe7,0xad,0x35,0x85,0xe2,0xf9,0x37,0xe8,0x1c,0x75,0xdf,0x6e,
    0x47,0xf1,0x1a,0x71,0x1d,0x29,0xc5,0x89,0x6f,0xb7,0x62,0x0e,0xaa,0x18,0xbe,0x1b,
    0xfc,0x56,0x3e,0x4b,0xc6,0xd2,0x79,0x20,0x9a,0xdb,0xc0,0xfe,0x78,0xcd,0x5a,0xf4,
    0x1f,0xdd,0xa8,0x33,0x88,0x07,0xc7,0x31,0xb1,0x12,0x10,0x59,0x27,0x80,0xec,0x5f,
    0x60,0x51,0x7f,0xa9,0x19,0xb5,0x4a,0x0d,0x2d,0xe5,0x7a,0x9f,0x93,0xc9,0x9c,0xef,
    0xa0,0xe0,0x3b,0x4d,0xae,0x2a,0xf5,0xb0,0xc8,0xeb,0xbb,0x3c,0x83,0x53,0x99,0x61,
    0x17,0x2b,0x04,0x7e,0xba,0x77,0xd6,0x26,0xe1,0x69,0x14,0x63,0x55,0x21,0x0c,0x7d,
];

const RCON: [u8; 11] = [
    0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36,
];

fn xtime(x: u8) -> u8 {
    (x << 1) ^ if (x & 0x80) != 0 { 0x1b } else { 0 }
}

fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut res = 0u8;
    for _ in 0..8 {
        if (b & 1) != 0 {
            res ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    res
}

fn aes_key_expand(key: &[u8]) -> Vec<[u8; 16]> {
    let nk = key.len() / 4;
    let nr = nk + 6;
    let total_words = 4 * (nr + 1);
    let mut w = vec![0u8; total_words * 4];
    w[..key.len()].copy_from_slice(key);
    for i in nk..total_words {
        let mut temp = [w[i * 4 - 4], w[i * 4 - 3], w[i * 4 - 2], w[i * 4 - 1]];
        if i % nk == 0 {
            temp.rotate_left(1);
            for b in &mut temp {
                *b = AES_SBOX[usize::from(*b)];
            }
            temp[0] ^= RCON[i / nk];
        } else if nk > 6 && i % nk == 4 {
            for b in &mut temp {
                *b = AES_SBOX[usize::from(*b)];
            }
        }
        for j in 0..4 {
            w[i * 4 + j] = w[(i - nk) * 4 + j] ^ temp[j];
        }
    }
    w.as_chunks::<16>().0.to_vec()
}

fn aes_encrypt_block(round_keys: &[[u8; 16]], block: &[u8]) -> [u8; 16] {
    let mut s = [0u8; 16];
    s.copy_from_slice(block);
    let nr = round_keys.len() - 1;
    for i in 0..16 {
        s[i] ^= round_keys[0][i];
    }
    for rk in &round_keys[1..nr] {
        for b in &mut s {
            *b = AES_SBOX[usize::from(*b)];
        }
        // ShiftRows
        let t = s;
        s[1] = t[5];
        s[5] = t[9];
        s[9] = t[13];
        s[13] = t[1];
        s[2] = t[10];
        s[6] = t[14];
        s[10] = t[2];
        s[14] = t[6];
        s[3] = t[15];
        s[7] = t[3];
        s[11] = t[7];
        s[15] = t[11];
        // MixColumns
        for col in 0..4 {
            let c = col * 4;
            let (a0, a1, a2, a3) = (s[c], s[c + 1], s[c + 2], s[c + 3]);
            s[c] = xtime(a0) ^ xtime(a1) ^ a1 ^ a2 ^ a3;
            s[c + 1] = a0 ^ xtime(a1) ^ xtime(a2) ^ a2 ^ a3;
            s[c + 2] = a0 ^ a1 ^ xtime(a2) ^ xtime(a3) ^ a3;
            s[c + 3] = xtime(a0) ^ a0 ^ a1 ^ a2 ^ xtime(a3);
        }
        for i in 0..16 {
            s[i] ^= rk[i];
        }
    }
    for b in &mut s {
        *b = AES_SBOX[usize::from(*b)];
    }
    let t = s;
    s[1] = t[5];
    s[5] = t[9];
    s[9] = t[13];
    s[13] = t[1];
    s[2] = t[10];
    s[6] = t[14];
    s[10] = t[2];
    s[14] = t[6];
    s[3] = t[15];
    s[7] = t[3];
    s[11] = t[7];
    s[15] = t[11];
    for i in 0..16 {
        s[i] ^= round_keys[nr][i];
    }
    s
}

fn aes_decrypt_block(round_keys: &[[u8; 16]], block: &[u8]) -> [u8; 16] {
    let mut s = [0u8; 16];
    s.copy_from_slice(block);
    let nr = round_keys.len() - 1;
    for i in 0..16 {
        s[i] ^= round_keys[nr][i];
    }
    for rk in round_keys[1..nr].iter().rev() {
        // InvShiftRows
        let t = s;
        s[1] = t[13];
        s[5] = t[1];
        s[9] = t[5];
        s[13] = t[9];
        s[2] = t[10];
        s[6] = t[14];
        s[10] = t[2];
        s[14] = t[6];
        s[3] = t[7];
        s[7] = t[11];
        s[11] = t[15];
        s[15] = t[3];
        for b in &mut s {
            *b = AES_INV_SBOX[usize::from(*b)];
        }
        for i in 0..16 {
            s[i] ^= rk[i];
        }
        // InvMixColumns
        for col in 0..4 {
            let c = col * 4;
            let (a0, a1, a2, a3) = (s[c], s[c + 1], s[c + 2], s[c + 3]);
            s[c] = gf_mul(a0, 14) ^ gf_mul(a1, 11) ^ gf_mul(a2, 13) ^ gf_mul(a3, 9);
            s[c + 1] = gf_mul(a0, 9) ^ gf_mul(a1, 14) ^ gf_mul(a2, 11) ^ gf_mul(a3, 13);
            s[c + 2] = gf_mul(a0, 13) ^ gf_mul(a1, 9) ^ gf_mul(a2, 14) ^ gf_mul(a3, 11);
            s[c + 3] = gf_mul(a0, 11) ^ gf_mul(a1, 13) ^ gf_mul(a2, 9) ^ gf_mul(a3, 14);
        }
    }
    let t = s;
    s[1] = t[13];
    s[5] = t[1];
    s[9] = t[5];
    s[13] = t[9];
    s[2] = t[10];
    s[6] = t[14];
    s[10] = t[2];
    s[14] = t[6];
    s[3] = t[7];
    s[7] = t[11];
    s[11] = t[15];
    s[15] = t[3];
    for b in &mut s {
        *b = AES_INV_SBOX[usize::from(*b)];
    }
    for i in 0..16 {
        s[i] ^= round_keys[0][i];
    }
    s
}

pub(crate) fn aes_128_cbc_encrypt_no_pad(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let rks = aes_key_expand(key);
    let mut prev = *iv;
    let mut out = Vec::with_capacity(data.len());
    for blk in data.as_chunks::<16>().0 {
        let mut x = [0u8; 16];
        for i in 0..16 {
            x[i] = blk[i] ^ prev[i];
        }
        let c = aes_encrypt_block(&rks, &x);
        out.extend_from_slice(&c);
        prev = c;
    }
    out
}

pub(crate) fn aes_128_cbc_decrypt(key: &[u8; 16], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = aes_cbc_decrypt_no_pad(key, iv, data);
    strip_pkcs7_padding(&mut out);
    out
}

pub(crate) fn aes_256_cbc_decrypt(key: &[u8; 32], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = aes_cbc_decrypt_no_pad(key, iv, data);
    strip_pkcs7_padding(&mut out);
    out
}

fn aes_256_cbc_decrypt_no_pad(key: &[u8; 32], iv: &[u8], data: &[u8]) -> Vec<u8> {
    aes_cbc_decrypt_no_pad(key, iv, data)
}

fn aes_cbc_decrypt_no_pad(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    if iv.len() < 16 || data.len() < 16 {
        return Vec::new();
    }
    let rks = aes_key_expand(key);
    let mut prev = [0u8; 16];
    prev.copy_from_slice(&iv[..16]);
    let mut out = Vec::with_capacity(data.len());
    for blk in data.as_chunks::<16>().0 {
        let p = aes_decrypt_block(&rks, blk);
        for i in 0..16 {
            out.push(p[i] ^ prev[i]);
        }
        prev.copy_from_slice(blk);
    }
    out
}

fn strip_pkcs7_padding(data: &mut Vec<u8>) {
    let Some(&pad) = data.last() else {
        return;
    };
    let pad_len = usize::from(pad);
    if (1..=16).contains(&pad_len)
        && data.len() >= pad_len
        && data[data.len() - pad_len..].iter().all(|&b| b == pad)
    {
        data.truncate(data.len() - pad_len);
    }
}

// =============================================================================
// Pure-Rust MD5, SHA-256, SHA-384, SHA-512
// =============================================================================

#[allow(clippy::many_single_char_names)]
pub(crate) fn md5(data: &[u8]) -> [u8; 16] {
    #[rustfmt::skip]
    const S: [u32; 64] = [
        7,12,17,22, 7,12,17,22, 7,12,17,22, 7,12,17,22,
        5, 9,14,20, 5, 9,14,20, 5, 9,14,20, 5, 9,14,20,
        4,11,16,23, 4,11,16,23, 4,11,16,23, 4,11,16,23,
        6,10,15,21, 6,10,15,21, 6,10,15,21, 6,10,15,21,
    ];
    #[rustfmt::skip]
    const K: [u32; 64] = [
        0xd76aa478,0xe8c7b756,0x242070db,0xc1bdceee,0xf57c0faf,0x4787c62a,0xa8304613,0xfd469501,
        0x698098d8,0x8b44f7af,0xffff5bb1,0x895cd7be,0x6b901122,0xfd987193,0xa679438e,0x49b40821,
        0xf61e2562,0xc040b340,0x265e5a51,0xe9b6c7aa,0xd62f105d,0x02441453,0xd8a1e681,0xe7d3fbc8,
        0x21e1cde6,0xc33707d6,0xf4d50d87,0x455a14ed,0xa9e3e905,0xfcefa3f8,0x676f02d9,0x8d2a4c8a,
        0xfffa3942,0x8771f681,0x6d9d6122,0xfde5380c,0xa4beea44,0x4bdecfa9,0xf6bb4b60,0xbebfbc70,
        0x289b7ec6,0xeaa127fa,0xd4ef3085,0x04881d05,0xd9d4d039,0xe6db99e5,0x1fa27cf8,0xc4ac5665,
        0xf4292244,0x432aff97,0xab9423a7,0xfc93a039,0x655b59c3,0x8f0ccc92,0xffeff47d,0x85845dd1,
        0x6fa87e4f,0xfe2ce6e0,0xa3014314,0x4e0811a1,0xf7537e82,0xbd3af235,0x2ad7d2bb,0xeb86d391,
    ];

    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) = (
        0x6745_2301u32,
        0xefcd_ab89u32,
        0x98ba_dcfeu32,
        0x1032_5476u32,
    );
    for chunk in msg.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[i])
                    .wrapping_add(m[g])
                    .rotate_left(S[i]),
            );
            a = temp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    #[rustfmt::skip]
    const K: [u32; 64] = [
        0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
        0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
        0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
        0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
        0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
        0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
        0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
        0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        let add = [a, b, c, d, e, f, g, hh];
        for i in 0..8 {
            h[i] = h[i].wrapping_add(add[i]);
        }
    }
    let mut out = [0u8; 32];
    for (i, &w) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
    }
    out
}

#[rustfmt::skip]
const K64: [u64; 80] = [
    0x428a2f98d728ae22,0x7137449123ef65cd,0xb5c0fbcfec4d3b2f,0xe9b5dba58189dbbc,
    0x3956c25bf348b538,0x59f111f1b605d019,0x923f82a4af194f9b,0xab1c5ed5da6d8118,
    0xd807aa98a3030242,0x12835b0145706fbe,0x243185be4ee4b28c,0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f,0x80deb1fe3b1696b1,0x9bdc06a725c71235,0xc19bf174cf692694,
    0xe49b69c19ef14ad2,0xefbe4786384f25e3,0x0fc19dc68b8cd5b5,0x240ca1cc77ac9c65,
    0x2de92c6f592b0275,0x4a7484aa6ea6e483,0x5cb0a9dcbd41fbd4,0x76f988da831153b5,
    0x983e5152ee66dfab,0xa831c66d2db43210,0xb00327c898fb213f,0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2,0xd5a79147930aa725,0x06ca6351e003826f,0x142929670a0e6e70,
    0x27b70a8546d22ffc,0x2e1b21385c26c926,0x4d2c6dfc5ac42aed,0x53380d139d95b3df,
    0x650a73548baf63de,0x766a0abb3c77b2a8,0x81c2c92e47edaee6,0x92722c851482353b,
    0xa2bfe8a14cf10364,0xa81a664bbc423001,0xc24b8b70d0f89791,0xc76c51a30654be30,
    0xd192e819d6ef5218,0xd69906245565a910,0xf40e35855771202a,0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8,0x1e376c085141ab53,0x2748774cdf8eeb99,0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63,0x4ed8aa4ae3418acb,0x5b9cca4f7763e373,0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc,0x78a5636f43172f60,0x84c87814a1f0ab72,0x8cc702081a6439ec,
    0x90befffa23631e28,0xa4506cebde82bde9,0xbef9a3f7b2c67915,0xc67178f2e372532b,
    0xca273eceea26619c,0xd186b8c721c0c207,0xeada7dd6cde0eb1e,0xf57d4f7fee6ed178,
    0x06f067aa72176fba,0x0a637dc5a2c898a6,0x113f9804bef90dae,0x1b710b35131c471b,
    0x28db77f523047d84,0x32caab7b40c72493,0x3c9ebe0a15c9bebc,0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6,0x597f299cfc657e2a,0x5fcb6fab3ad6faec,0x6c44198c4a475817,
];

fn sha512_core(data: &[u8], init: [u64; 8]) -> [u64; 8] {
    let mut h = init;
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u128).wrapping_mul(8);
    msg.push(0x80);
    while (msg.len() % 128) != 112 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.as_chunks::<128>().0 {
        let mut w = [0u64; 80];
        for i in 0..16 {
            w[i] = u64::from_be_bytes([
                chunk[i * 8],
                chunk[i * 8 + 1],
                chunk[i * 8 + 2],
                chunk[i * 8 + 3],
                chunk[i * 8 + 4],
                chunk[i * 8 + 5],
                chunk[i * 8 + 6],
                chunk[i * 8 + 7],
            ]);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K64[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        let add = [a, b, c, d, e, f, g, hh];
        for i in 0..8 {
            h[i] = h[i].wrapping_add(add[i]);
        }
    }
    h
}

pub(crate) fn sha384(data: &[u8]) -> [u8; 48] {
    let h = sha512_core(
        data,
        [
            0xcbbb_9d5d_c105_9ed8,
            0x629a_292a_367c_d507,
            0x9159_015a_3070_dd17,
            0x152f_ecd8_f70e_5939,
            0x6733_2667_ffc0_0b31,
            0x8eb4_4a87_6858_1511,
            0xdb0c_2e0d_64f9_8fa7,
            0x47b5_481d_befa_4fa4,
        ],
    );
    let mut out = [0u8; 48];
    for i in 0..6 {
        out[i * 8..i * 8 + 8].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

pub(crate) fn sha512(data: &[u8]) -> [u8; 64] {
    let h = sha512_core(
        data,
        [
            0x6a09_e667_f3bc_c908,
            0xbb67_ae85_84ca_a73b,
            0x3c6e_f372_fe94_f82b,
            0xa54f_f53a_5f1d_36f1,
            0x510e_527f_ade6_82d1,
            0x9b05_688c_2b3e_6c1f,
            0x1f83_d9ab_fb41_bd6b,
            0x5be0_cd19_137e_2179,
        ],
    );
    let mut out = [0u8; 64];
    for (i, &w) in h.iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&w.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_rust_hashes_and_ciphers_match_known_test_vectors() {
        // RFC 1321 MD5("abc")
        assert_eq!(
            md5(b"abc"),
            [
                0x90, 0x01, 0x50, 0x98, 0x3c, 0xd2, 0x4f, 0xb0, 0xd6, 0x96, 0x3f, 0x7d, 0x28, 0xe1,
                0x7f, 0x72
            ]
        );
        // FIPS 180-4 SHA-256("abc")
        assert_eq!(
            sha256(b"abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
        // AES-128-CBC roundtrip
        let key = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let iv = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let plain = b"PDF AES128 test!"; // 16 bytes
        let mut padded = plain.to_vec();
        padded.extend_from_slice(&[16u8; 16]);
        let ct = aes_128_cbc_encrypt_no_pad(&key, &iv, &padded);
        let dec = aes_128_cbc_decrypt(&key, &iv, &ct);
        assert_eq!(dec, plain);
    }
}
