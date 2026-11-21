// SPDX-License-Identifier: MPL-2.0

//! Clean-room RIFF / RIFX binary container reader and writer, including
//! Corel `LIST cmpr` Zlib-compressed block streams with `CPng` chunk-size tables.

use crate::error::{CdrError, CdrResult};
use crate::limits::{decompress_zlib_bounded, ByteReader, CodecLimits};
use flate2::{write::ZlibEncoder, Compression};
use std::io::Write;

/// Byte order of a RIFF (`RIFF` = little-endian) or RIFX (`RIFX` = big-endian) container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RiffEndian {
    Little,
    Big,
}

impl RiffEndian {
    /// Four-byte root magic (`b"RIFF"` or `b"RIFX"`).
    #[must_use]
    pub const fn magic(self) -> [u8; 4] {
        match self {
            Self::Little => *b"RIFF",
            Self::Big => *b"RIFX",
        }
    }

    pub(crate) fn read_u16(self, reader: &mut ByteReader<'_>, ctx: &'static str) -> CdrResult<u16> {
        match self {
            Self::Little => reader.read_u16_le(ctx),
            Self::Big => reader.read_u16_be(ctx),
        }
    }

    pub(crate) fn read_i16(self, reader: &mut ByteReader<'_>, ctx: &'static str) -> CdrResult<i16> {
        match self {
            Self::Little => reader.read_i16_le(ctx),
            Self::Big => reader.read_i16_be(ctx),
        }
    }

    pub(crate) fn read_u32(self, reader: &mut ByteReader<'_>, ctx: &'static str) -> CdrResult<u32> {
        match self {
            Self::Little => reader.read_u32_le(ctx),
            Self::Big => reader.read_u32_be(ctx),
        }
    }

    pub(crate) fn read_i32(self, reader: &mut ByteReader<'_>, ctx: &'static str) -> CdrResult<i32> {
        match self {
            Self::Little => reader.read_i32_le(ctx),
            Self::Big => reader.read_i32_be(ctx),
        }
    }

    pub(crate) fn read_f32(self, reader: &mut ByteReader<'_>, ctx: &'static str) -> CdrResult<f32> {
        match self {
            Self::Little => reader.read_f32_le(ctx),
            Self::Big => reader.read_f32_be(ctx),
        }
    }

    pub(crate) fn read_f64(self, reader: &mut ByteReader<'_>, ctx: &'static str) -> CdrResult<f64> {
        match self {
            Self::Little => reader.read_f64_le(ctx),
            Self::Big => reader.read_f64_be(ctx),
        }
    }

    pub(crate) fn write_u16(self, out: &mut Vec<u8>, v: u16) {
        match self {
            Self::Little => out.extend_from_slice(&v.to_le_bytes()),
            Self::Big => out.extend_from_slice(&v.to_be_bytes()),
        }
    }

    pub(crate) fn write_i16(self, out: &mut Vec<u8>, v: i16) {
        match self {
            Self::Little => out.extend_from_slice(&v.to_le_bytes()),
            Self::Big => out.extend_from_slice(&v.to_be_bytes()),
        }
    }

    pub(crate) fn write_u32(self, out: &mut Vec<u8>, v: u32) {
        match self {
            Self::Little => out.extend_from_slice(&v.to_le_bytes()),
            Self::Big => out.extend_from_slice(&v.to_be_bytes()),
        }
    }

    pub(crate) fn write_i32(self, out: &mut Vec<u8>, v: i32) {
        match self {
            Self::Little => out.extend_from_slice(&v.to_le_bytes()),
            Self::Big => out.extend_from_slice(&v.to_be_bytes()),
        }
    }

    pub(crate) fn write_f32(self, out: &mut Vec<u8>, v: f32) {
        match self {
            Self::Little => out.extend_from_slice(&v.to_le_bytes()),
            Self::Big => out.extend_from_slice(&v.to_be_bytes()),
        }
    }

    pub(crate) fn write_f64(self, out: &mut Vec<u8>, v: f64) {
        match self {
            Self::Little => out.extend_from_slice(&v.to_le_bytes()),
            Self::Big => out.extend_from_slice(&v.to_be_bytes()),
        }
    }
}

/// Converts a 4-byte FourCC into a printable ASCII/lossy string.
#[must_use]
pub fn fourcc_to_string(fourcc: &[u8; 4]) -> String {
    fourcc
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

/// A single node inside a parsed RIFF/RIFX hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiffNode {
    /// Leaf data chunk (`fourcc` + raw payload bytes).
    Chunk {
        fourcc: [u8; 4],
        data: Vec<u8>,
        /// `true` if this chunk was unpacked from inside a `LIST cmpr` Zlib stream.
        compressed: bool,
    },
    /// Nested `LIST` container (`list_type` + ordered child nodes).
    List {
        list_type: [u8; 4],
        children: Vec<RiffNode>,
        /// `true` if this list is a `cmpr` block or resides inside one.
        compressed: bool,
    },
}

impl RiffNode {
    /// Convenience constructor for an uncompressed leaf chunk.
    #[must_use]
    pub fn chunk(fourcc: [u8; 4], data: Vec<u8>) -> Self {
        Self::Chunk {
            fourcc,
            data,
            compressed: false,
        }
    }

    /// Convenience constructor for an uncompressed `LIST` node.
    #[must_use]
    pub fn list(list_type: [u8; 4], children: Vec<Self>) -> Self {
        Self::List {
            list_type,
            children,
            compressed: false,
        }
    }

    /// Convenience constructor for a compressed `LIST cmpr` block.
    #[must_use]
    pub fn cmpr_list(children: Vec<Self>) -> Self {
        Self::List {
            list_type: *b"cmpr",
            children,
            compressed: true,
        }
    }

    /// Returns the FourCC of a leaf chunk or the `list_type` of a `LIST`.
    #[must_use]
    pub const fn tag(&self) -> [u8; 4] {
        match self {
            Self::Chunk { fourcc, .. } => *fourcc,
            Self::List { list_type, .. } => *list_type,
        }
    }

    /// Returns `true` if this node is a `LIST`.
    #[must_use]
    pub const fn is_list(&self) -> bool {
        matches!(self, Self::List { .. })
    }

    /// Returns `true` if this node originated from or is a `LIST cmpr` block.
    #[must_use]
    pub const fn is_compressed(&self) -> bool {
        match self {
            Self::Chunk { compressed, .. } | Self::List { compressed, .. } => *compressed,
        }
    }

    /// Finds the first direct child leaf chunk matching `fourcc`.
    #[must_use]
    pub fn find_chunk(&self, fourcc: &[u8; 4]) -> Option<&[u8]> {
        match self {
            Self::List { children, .. } => children.iter().find_map(|c| match c {
                Self::Chunk {
                    fourcc: f, data, ..
                } if f == fourcc => Some(data.as_slice()),
                _ => None,
            }),
            Self::Chunk { .. } => None,
        }
    }

    /// Finds the first direct child `LIST` matching `list_type`.
    #[must_use]
    pub fn find_list(&self, list_type: &[u8; 4]) -> Option<&[Self]> {
        match self {
            Self::List { children, .. } => children.iter().find_map(|c| match c {
                Self::List {
                    list_type: lt,
                    children: ch,
                    ..
                } if lt == list_type => Some(ch.as_slice()),
                _ => None,
            }),
            Self::Chunk { .. } => None,
        }
    }
}

/// Parsed root RIFF/RIFX container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiffDocument {
    pub endian: RiffEndian,
    pub form_type: [u8; 4],
    pub children: Vec<RiffNode>,
}

impl RiffDocument {
    /// Creates a new little-endian `RIFF` document.
    #[must_use]
    pub fn new(form_type: [u8; 4], children: Vec<RiffNode>) -> Self {
        Self {
            endian: RiffEndian::Little,
            form_type,
            children,
        }
    }

    /// Finds the first top-level leaf chunk matching `fourcc`.
    #[must_use]
    pub fn find_chunk(&self, fourcc: &[u8; 4]) -> Option<&[u8]> {
        self.children.iter().find_map(|c| match c {
            RiffNode::Chunk {
                fourcc: f, data, ..
            } if f == fourcc => Some(data.as_slice()),
            _ => None,
        })
    }

    /// Finds the first top-level `LIST` matching `list_type`.
    #[must_use]
    pub fn find_list(&self, list_type: &[u8; 4]) -> Option<&[RiffNode]> {
        self.children.iter().find_map(|c| match c {
            RiffNode::List {
                list_type: lt,
                children,
                ..
            } if lt == list_type => Some(children.as_slice()),
            _ => None,
        })
    }
}

/// Parses a RIFF or RIFX byte buffer with strict depth, chunk-count, and decompression guards.
pub fn parse_riff(bytes: &[u8], limits: &CodecLimits) -> CdrResult<RiffDocument> {
    let mut budget = limits.max_uncompressed_bytes;
    parse_riff_with_budget(bytes, limits, &mut budget, true)
}

/// Parses a RIFF or RIFX byte buffer while deducting from a shared cumulative
/// `budget_remaining` across container layers (e.g. ZIP + `LIST cmpr` + bitmaps).
///
/// Set `charge_input_bytes = false` only when `bytes` has already been charged to
/// `budget_remaining` by an outer container reader (such as `parse_cdr_zip`).
pub fn parse_riff_with_budget(
    bytes: &[u8],
    limits: &CodecLimits,
    budget_remaining: &mut u64,
    charge_input_bytes: bool,
) -> CdrResult<RiffDocument> {
    if charge_input_bytes {
        limits.consume_budget(budget_remaining, bytes.len() as u64)?;
    }

    let mut reader = ByteReader::new(bytes);
    let magic = reader.read_fourcc("RIFF magic header")?;
    let endian = match &magic {
        b"RIFF" => RiffEndian::Little,
        b"RIFX" => RiffEndian::Big,
        _ => {
            return Err(CdrError::InvalidMagic {
                expected: "RIFF or RIFX".to_owned(),
                found: fourcc_to_string(&magic),
            });
        }
    };

    let declared_size = endian.read_u32(&mut reader, "RIFF root size")?;
    if declared_size < 4 {
        return Err(CdrError::MalformedHeader(format!(
            "RIFF root size {declared_size} is smaller than 4-byte form type"
        )));
    }
    let body_len = usize::try_from(declared_size)
        .map_err(|_| CdrError::ArithmeticOverflow("RIFF body size"))?;
    let body = reader.read_bytes(body_len, "RIFF root body")?;

    let mut body_reader = ByteReader::new(body);
    let form_type = body_reader.read_fourcc("RIFF form type")?;
    let rest = body_reader.read_rest();

    let mut chunk_count = 0usize;
    let children = parse_chunk_sequence(
        rest,
        endian,
        limits,
        1,
        &mut chunk_count,
        budget_remaining,
        None,
        false,
    )?;

    Ok(RiffDocument {
        endian,
        form_type,
        children,
    })
}

/// Serializes the body of a `LIST` (`list_type` + all nested child chunks/lists)
/// so unrecognized `LIST` containers can be preserved verbatim in `UnknownCdrChunk`.
pub fn serialize_list_body(
    list_type: &[u8; 4],
    children: &[RiffNode],
    endian: RiffEndian,
) -> CdrResult<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(list_type);
    for child in children {
        write_node(&mut out, child, endian, None)?;
    }
    Ok(out)
}

/// Re-parses a serialized `LIST` body (`list_type` + child chunks/lists) into a [`RiffNode::List`]
/// so preserved unknown `LIST` chunks re-emit cleanly inside both uncompressed and `LIST cmpr` blocks.
pub fn parse_list_body(
    raw_list_body: &[u8],
    endian: RiffEndian,
    limits: &CodecLimits,
) -> CdrResult<RiffNode> {
    let mut r = ByteReader::new(raw_list_body);
    let list_type = r.read_fourcc("unknown LIST type")?;
    let rest = r.read_rest();
    let mut chunk_count = 0usize;
    let mut budget = limits.max_uncompressed_bytes;
    let children = parse_chunk_sequence(
        rest,
        endian,
        limits,
        1,
        &mut chunk_count,
        &mut budget,
        None,
        false,
    )?;
    Ok(RiffNode::List {
        list_type,
        children,
        compressed: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn parse_chunk_sequence(
    bytes: &[u8],
    endian: RiffEndian,
    limits: &CodecLimits,
    depth: usize,
    chunk_count: &mut usize,
    budget_remaining: &mut u64,
    size_table: Option<&[u32]>,
    in_compressed: bool,
) -> CdrResult<Vec<RiffNode>> {
    limits.check_depth(depth)?;
    let mut reader = ByteReader::new(bytes);
    let mut nodes = Vec::new();

    while !reader.is_eof() {
        // Ignore a single trailing 0x00 alignment pad byte if nothing else fits
        if reader.remaining() < 8 {
            if reader.remaining() == 1 && bytes.get(reader.position()) == Some(&0) {
                break;
            }
            return Err(CdrError::UnexpectedEof {
                context: "RIFF chunk header",
                needed: 8,
                available: reader.remaining(),
            });
        }

        limits.check_and_inc_chunks(chunk_count)?;
        let fourcc = reader.read_fourcc("chunk FourCC")?;
        let raw_size_field = endian.read_u32(&mut reader, "chunk size")?;

        let chunk_size_u32 = match size_table {
            Some(table) => {
                let idx = usize::try_from(raw_size_field)
                    .map_err(|_| CdrError::ArithmeticOverflow("CPng size table index"))?;
                *table.get(idx).ok_or_else(|| CdrError::InvalidChunk {
                    fourcc: fourcc_to_string(&fourcc),
                    reason: format!(
                        "CPng size table index {idx} out of bounds (len {})",
                        table.len()
                    ),
                })?
            }
            None => raw_size_field,
        };

        let chunk_size = usize::try_from(chunk_size_u32)
            .map_err(|_| CdrError::ArithmeticOverflow("chunk size"))?;
        let payload = reader.read_bytes(chunk_size, "RIFF chunk payload")?;

        // RIFF 2-byte word alignment padding for odd-sized chunks
        if chunk_size % 2 == 1 && !reader.is_eof() {
            reader.skip(1, "RIFF word-alignment pad byte")?;
        }

        if &fourcc == b"LIST" {
            if payload.len() < 4 {
                return Err(CdrError::InvalidChunk {
                    fourcc: "LIST".to_owned(),
                    reason: "LIST chunk shorter than 4-byte list type".to_owned(),
                });
            }
            let mut list_reader = ByteReader::new(payload);
            let list_type = list_reader.read_fourcc("LIST type")?;
            let list_body = list_reader.read_rest();

            if &list_type == b"cmpr" {
                let cmpr_children = parse_cmpr_body(
                    list_body,
                    endian,
                    limits,
                    depth + 1,
                    chunk_count,
                    budget_remaining,
                )?;
                nodes.push(RiffNode::List {
                    list_type,
                    children: cmpr_children,
                    compressed: true,
                });
            } else {
                let children = parse_chunk_sequence(
                    list_body,
                    endian,
                    limits,
                    depth + 1,
                    chunk_count,
                    budget_remaining,
                    size_table,
                    in_compressed,
                )?;
                nodes.push(RiffNode::List {
                    list_type,
                    children,
                    compressed: in_compressed,
                });
            }
        } else {
            nodes.push(RiffNode::Chunk {
                fourcc,
                data: payload.to_vec(),
                compressed: in_compressed,
            });
        }
    }

    Ok(nodes)
}

/// Parses the body of a Corel `LIST cmpr` Zlib-compressed chunk block.
///
/// Layout after `LIST <size> cmpr`:
/// - `u32` compressed stream byte length
/// - `u32` declared uncompressed stream length (hint only; bounded decompression is authoritative)
/// - `u32` compressed `CPng` chunk-size table byte length (`0` if direct sizes are embedded)
/// - `[u8; 4]` signature (`b"CPng"` or zeros)
/// - `u16` compression method (`1` = Zlib)
/// - `u16` reserved flags
/// - `[u8; compressed_len]` Zlib-compressed chunk sequence
/// - `[u8; blocks_len]` optional Zlib-compressed `Vec<u32>` chunk-size lookup table
fn parse_cmpr_body(
    body: &[u8],
    endian: RiffEndian,
    limits: &CodecLimits,
    depth: usize,
    chunk_count: &mut usize,
    budget_remaining: &mut u64,
) -> CdrResult<Vec<RiffNode>> {
    limits.check_depth(depth)?;
    let mut reader = ByteReader::new(body);

    let compressed_len = usize::try_from(endian.read_u32(&mut reader, "cmpr compressed_size")?)
        .map_err(|_| CdrError::ArithmeticOverflow("cmpr compressed_size"))?;
    let _declared_uncompressed = endian.read_u32(&mut reader, "cmpr uncompressed_size")?;
    let blocks_len = usize::try_from(endian.read_u32(&mut reader, "cmpr blocks_info_size")?)
        .map_err(|_| CdrError::ArithmeticOverflow("cmpr blocks_info_size"))?;
    let _sig = reader.read_fourcc("cmpr CPng signature")?;
    let _method = endian.read_u16(&mut reader, "cmpr method")?;
    let _reserved = endian.read_u16(&mut reader, "cmpr reserved")?;

    let compressed_stream = reader.read_bytes(compressed_len, "cmpr Zlib stream")?;
    let blocks_stream = if blocks_len > 0 {
        Some(reader.read_bytes(blocks_len, "cmpr CPng blocks table")?)
    } else {
        None
    };

    let uncompressed_stream = decompress_zlib_bounded(compressed_stream, limits, budget_remaining)?;

    let size_table: Option<Vec<u32>> = match blocks_stream {
        Some(blk_bytes) => {
            let raw_table = decompress_zlib_bounded(blk_bytes, limits, budget_remaining)?;
            if !raw_table.len().is_multiple_of(4) {
                return Err(CdrError::InvalidChunk {
                    fourcc: "CPng".to_owned(),
                    reason: format!(
                        "CPng chunk-size table byte length {} is not a multiple of 4",
                        raw_table.len()
                    ),
                });
            }
            let count = raw_table.len() / 4;
            let mut tr = ByteReader::new(&raw_table);
            let mut table = Vec::with_capacity(count);
            for _ in 0..count {
                table.push(endian.read_u32(&mut tr, "CPng size entry")?);
            }
            Some(table)
        }
        None => None,
    };

    parse_chunk_sequence(
        &uncompressed_stream,
        endian,
        limits,
        depth,
        chunk_count,
        budget_remaining,
        size_table.as_deref(),
        true,
    )
}

/// Serializes a [`RiffDocument`] into valid RIFF/RIFX bytes with 2-byte word alignment
/// and automatic `LIST cmpr` Zlib + `CPng` compression.
pub fn write_riff(doc: &RiffDocument) -> CdrResult<Vec<u8>> {
    let mut body = Vec::new();
    body.extend_from_slice(&doc.form_type);
    for node in &doc.children {
        write_node(&mut body, node, doc.endian, None)?;
    }

    let body_size = u32::try_from(body.len())
        .map_err(|_| CdrError::ArithmeticOverflow("RIFF total size exceeds u32"))?;

    let mut out = Vec::with_capacity(body.len().saturating_add(8));
    out.extend_from_slice(&doc.endian.magic());
    doc.endian.write_u32(&mut out, body_size);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Encodes `children` into the payload of a Corel `LIST cmpr` block (excluding the
/// outer `"LIST"`, size, and `"cmpr"` bytes).
pub fn encode_cmpr_body(
    children: &[RiffNode],
    endian: RiffEndian,
    use_cpng_table: bool,
) -> CdrResult<Vec<u8>> {
    let mut raw_stream = Vec::new();
    let mut size_table: Option<Vec<u32>> = if use_cpng_table {
        Some(Vec::new())
    } else {
        None
    };

    for child in children {
        write_node(&mut raw_stream, child, endian, size_table.as_mut())?;
    }

    let compressed_stream = zlib_compress(&raw_stream)?;
    let compressed_blocks = match size_table {
        Some(table) => {
            let mut raw_table = Vec::with_capacity(table.len().saturating_mul(4));
            for sz in table {
                endian.write_u32(&mut raw_table, sz);
            }
            zlib_compress(&raw_table)?
        }
        None => Vec::new(),
    };

    let comp_len = u32::try_from(compressed_stream.len())
        .map_err(|_| CdrError::ArithmeticOverflow("cmpr compressed size"))?;
    let uncomp_len = u32::try_from(raw_stream.len())
        .map_err(|_| CdrError::ArithmeticOverflow("cmpr uncompressed size"))?;
    let blocks_len = u32::try_from(compressed_blocks.len())
        .map_err(|_| CdrError::ArithmeticOverflow("cmpr CPng blocks size"))?;

    let mut body = Vec::with_capacity(
        20usize
            .saturating_add(compressed_stream.len())
            .saturating_add(compressed_blocks.len()),
    );
    endian.write_u32(&mut body, comp_len);
    endian.write_u32(&mut body, uncomp_len);
    endian.write_u32(&mut body, blocks_len);
    body.extend_from_slice(b"CPng");
    endian.write_u16(&mut body, 1); // Zlib compression
    endian.write_u16(&mut body, 0); // reserved
    body.extend_from_slice(&compressed_stream);
    body.extend_from_slice(&compressed_blocks);
    Ok(body)
}

fn write_node(
    out: &mut Vec<u8>,
    node: &RiffNode,
    endian: RiffEndian,
    mut size_table: Option<&mut Vec<u32>>,
) -> CdrResult<()> {
    match node {
        RiffNode::Chunk { fourcc, data, .. } => {
            let chunk_size = u32::try_from(data.len())
                .map_err(|_| CdrError::ArithmeticOverflow("RIFF chunk size"))?;
            let size_field = match size_table {
                Some(ref mut table) => {
                    let idx = u32::try_from(table.len())
                        .map_err(|_| CdrError::ArithmeticOverflow("CPng table index"))?;
                    table.push(chunk_size);
                    idx
                }
                None => chunk_size,
            };
            out.extend_from_slice(fourcc);
            endian.write_u32(out, size_field);
            out.extend_from_slice(data);
            if data.len() % 2 == 1 {
                out.push(0);
            }
        }
        RiffNode::List {
            list_type,
            children,
            ..
        } => {
            if *list_type == *b"cmpr" {
                let cmpr_body = encode_cmpr_body(children, endian, true)?;
                let payload_len = cmpr_body
                    .len()
                    .checked_add(4)
                    .ok_or(CdrError::ArithmeticOverflow("LIST cmpr payload"))?;
                let payload_u32 = u32::try_from(payload_len)
                    .map_err(|_| CdrError::ArithmeticOverflow("LIST cmpr size"))?;
                let size_field = match size_table {
                    Some(ref mut table) => {
                        let idx = u32::try_from(table.len())
                            .map_err(|_| CdrError::ArithmeticOverflow("CPng table index"))?;
                        table.push(payload_u32);
                        idx
                    }
                    None => payload_u32,
                };
                out.extend_from_slice(b"LIST");
                endian.write_u32(out, size_field);
                out.extend_from_slice(b"cmpr");
                out.extend_from_slice(&cmpr_body);
                if payload_len % 2 == 1 {
                    out.push(0);
                }
            } else {
                // Reserve the size slot BEFORE recursing if using a CPng table so the parent
                // LIST's size index precedes its children's indices in sequential read order.
                let reserved_idx = if let Some(ref mut table) = size_table {
                    let idx = table.len();
                    table.push(0);
                    Some(idx)
                } else {
                    None
                };

                let mut inner = Vec::new();
                inner.extend_from_slice(list_type);
                for child in children {
                    write_node(&mut inner, child, endian, size_table.as_deref_mut())?;
                }

                let inner_u32 = u32::try_from(inner.len())
                    .map_err(|_| CdrError::ArithmeticOverflow("LIST size"))?;
                let size_field = match (reserved_idx, size_table) {
                    (Some(idx), Some(table)) => {
                        table[idx] = inner_u32;
                        u32::try_from(idx)
                            .map_err(|_| CdrError::ArithmeticOverflow("CPng index"))?
                    }
                    _ => inner_u32,
                };

                out.extend_from_slice(b"LIST");
                endian.write_u32(out, size_field);
                out.extend_from_slice(&inner);
                if inner.len() % 2 == 1 {
                    out.push(0);
                }
            }
        }
    }
    Ok(())
}

fn zlib_compress(raw: &[u8]) -> CdrResult<Vec<u8>> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(raw)
        .map_err(|e| CdrError::DecompressionFailed(e.to_string()))?;
    enc.finish()
        .map_err(|e| CdrError::DecompressionFailed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn riff_and_rifx_roundtrip_with_odd_padding_and_cmpr_cpng() {
        let doc = RiffDocument {
            endian: RiffEndian::Little,
            form_type: *b"CDRC",
            children: vec![
                RiffNode::chunk(*b"vrsn", 1200u16.to_le_bytes().to_vec()),
                // Odd length payload (5 bytes) must pad cleanly and strip pad on read
                RiffNode::chunk(*b"XODD", vec![1, 2, 3, 4, 5]),
                RiffNode::cmpr_list(vec![RiffNode::list(
                    *b"doc ",
                    vec![
                        RiffNode::chunk(*b"loda", vec![0xAA, 0xBB, 0xCC]),
                        RiffNode::chunk(*b"fild", vec![0x10, 0x20, 0x30, 0x40]),
                    ],
                )]),
            ],
        };

        let bytes = write_riff(&doc).unwrap();
        let parsed = parse_riff(&bytes, &CodecLimits::default()).unwrap();

        assert_eq!(parsed.form_type, *b"CDRC");
        assert_eq!(parsed.find_chunk(b"XODD"), Some(&[1u8, 2, 3, 4, 5][..]));
        let cmpr = parsed.find_list(b"cmpr").unwrap();
        assert_eq!(cmpr.len(), 1);
        assert!(cmpr[0].is_compressed());
        let doc_list = cmpr[0].find_chunk(b"loda").unwrap();
        assert_eq!(doc_list, &[0xAA, 0xBB, 0xCC]);

        // Big-endian RIFX roundtrip
        let mut be_doc = doc;
        be_doc.endian = RiffEndian::Big;
        let be_bytes = write_riff(&be_doc).unwrap();
        assert_eq!(&be_bytes[0..4], b"RIFX");
        let parsed_be = parse_riff(&be_bytes, &CodecLimits::default()).unwrap();
        assert_eq!(parsed_be.endian, RiffEndian::Big);
        assert_eq!(parsed_be.find_chunk(b"XODD"), Some(&[1u8, 2, 3, 4, 5][..]));
    }

    #[test]
    fn deep_nested_lists_trigger_depth_limit_without_stack_overflow() {
        let mut node = RiffNode::chunk(*b"leaf", vec![42]);
        for _ in 0..100 {
            node = RiffNode::list(*b"grp ", vec![node]);
        }
        let doc = RiffDocument::new(*b"CDRC", vec![node]);
        let bytes = write_riff(&doc).unwrap();

        let err = parse_riff(&bytes, &CodecLimits::default()).unwrap_err();
        assert!(matches!(err, CdrError::DepthLimitExceeded { .. }));
    }
}
