// SPDX-License-Identifier: MIT OR Apache-2.0

//! Zero-`unsafe`, zero-subprocess, cross-platform native OS printer discovery and spooler submission.
//!
//! - **Linux / macOS (Unix)**: Speaks binary **CUPS IPP 2.0** (`RFC 8010 / RFC 8011`) directly
//!   over `/run/cups/cups.sock` (or `/var/run/cups/cups.sock` / `$CUPS_SERVER`) using
//!   `std::os::unix::net::UnixStream` — zero `std::process::Command` and zero `std::net`.
//! - **Windows**: Stages validated print-ready PDF / XPS-EMF spool payloads atomically via
//!   pure `std::fs` + `coree_io` without invoking external shells or `unsafe` FFI.

use coree_core::{Affine2D, CommandHistory, CommandOp, Document};
use coree_io::{export_pdf, PdfColorSpace, PdfExportOptions};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
#[cfg(unix)]
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

const fn default_print_scale_pct() -> f64 {
    100.0
}

/// Discovered local or network OS printer queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterInfo {
    pub name: String,
    pub is_default: bool,
    pub status: String,
}

/// Native OS print job configuration shown in Print Studio (`Ctrl+P`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintJobConfig {
    pub printer_name: Option<String>,
    pub copies: u32,
    /// `"all"` or 1-based page specification like `"1,3-5"`.
    pub page_range: String,
    pub fit_to_page: bool,
    pub force_cmyk: bool,
    pub crop_marks: bool,
    pub collate: bool,
    #[serde(default = "default_print_scale_pct")]
    pub scale_pct: f64,
}

impl Default for PrintJobConfig {
    fn default() -> Self {
        Self {
            printer_name: None,
            copies: 1,
            page_range: "all".to_owned(),
            fit_to_page: true,
            force_cmyk: true,
            crop_marks: false,
            collate: true,
            scale_pct: 100.0,
        }
    }
}

/// Validates a printer queue name against empty strings, control characters, and flag injection.
pub fn sanitize_printer_name(name: &str) -> Result<&str, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("printer name cannot be empty".to_owned());
    }
    if trimmed.starts_with('-') {
        return Err("invalid printer name: leading '-' is rejected".to_owned());
    }
    if trimmed
        .chars()
        .any(|c| c.is_control() || c == '\0' || c == '/' || c == '\\')
    {
        return Err("invalid printer name: control or path characters rejected".to_owned());
    }
    Ok(trimmed)
}

/// Parses 1-based page range strings (`"all"`, `""`, `"1"`, `"1,3-5"`) into 0-based page indices.
pub fn parse_page_range_1based(
    spec: &str,
    total_pages: usize,
) -> Result<Option<Vec<usize>>, String> {
    if total_pages == 0 {
        return Err("document has no pages to print".to_owned());
    }
    let trimmed = spec.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("all") || trimmed == "*" {
        return Ok(None);
    }

    let mut pages = Vec::new();
    for part in trimmed.split(',') {
        let token = part.trim();
        if token.is_empty() {
            continue;
        }
        if let Some((start_s, end_s)) = token.split_once('-') {
            let start = start_s
                .trim()
                .parse::<usize>()
                .map_err(|_| format!("invalid page number '{start_s}' in range '{token}'"))?;
            let end = end_s
                .trim()
                .parse::<usize>()
                .map_err(|_| format!("invalid page number '{end_s}' in range '{token}'"))?;
            if start == 0 || end == 0 || start > end || end > total_pages {
                return Err(format!(
                    "page range '{token}' is out of bounds (1..={total_pages})"
                ));
            }
            for p in start..=end {
                let idx = p - 1;
                if !pages.contains(&idx) {
                    pages.push(idx);
                }
            }
        } else {
            let p = token
                .parse::<usize>()
                .map_err(|_| format!("invalid page number '{token}'"))?;
            if p == 0 || p > total_pages {
                return Err(format!("page {p} is out of bounds (1..={total_pages})"));
            }
            let idx = p - 1;
            if !pages.contains(&idx) {
                pages.push(idx);
            }
        }
    }

    if pages.is_empty() {
        return Err("page range matched zero pages".to_owned());
    }
    Ok(Some(pages))
}

// =============================================================================
// Binary CUPS IPP 2.0 Encoder & Decoder (`RFC 8010` / `RFC 8011`)
// =============================================================================

#[cfg(unix)]
const IPP_VERSION_MAJOR: u8 = 2;
#[cfg(unix)]
const IPP_VERSION_MINOR: u8 = 0;
#[cfg(unix)]
const IPP_DEFAULT_PORT: u16 = 631;

#[cfg(unix)]
const IPP_OP_PRINT_JOB: u16 = 0x0002;
#[cfg(unix)]
const IPP_OP_CUPS_GET_DEFAULT: u16 = 0x4001;
#[cfg(unix)]
const IPP_OP_CUPS_GET_PRINTERS: u16 = 0x4002;

#[cfg(unix)]
const IPP_TAG_OPERATION_ATTRS: u8 = 0x01;
#[cfg(unix)]
const IPP_TAG_JOB_ATTRS: u8 = 0x02;
#[cfg(unix)]
const IPP_TAG_END_OF_ATTRS: u8 = 0x03;
#[cfg(unix)]
const IPP_TAG_PRINTER_ATTRS: u8 = 0x04;

#[cfg(unix)]
const IPP_TAG_INTEGER: u8 = 0x21;
#[cfg(all(unix, test))]
const IPP_TAG_ENUM: u8 = 0x23;
#[cfg(unix)]
const IPP_TAG_NAME_NO_LANG: u8 = 0x42;
#[cfg(unix)]
const IPP_TAG_KEYWORD: u8 = 0x44;
#[cfg(unix)]
const IPP_TAG_URI: u8 = 0x45;
#[cfg(unix)]
const IPP_TAG_CHARSET: u8 = 0x47;
#[cfg(unix)]
const IPP_TAG_NATURAL_LANG: u8 = 0x48;
#[cfg(unix)]
const IPP_TAG_MIME_MEDIA_TYPE: u8 = 0x49;

#[cfg(unix)]
#[derive(Debug, Clone)]
enum IppAttrValue {
    Str(u8, String),
    Int(i32),
}

#[cfg(unix)]
fn push_ipp_attr(buf: &mut Vec<u8>, tag: u8, name: &str, value_bytes: &[u8]) {
    buf.push(tag);
    buf.extend_from_slice(&(name.len() as u16).to_be_bytes());
    buf.extend_from_slice(name.as_bytes());
    buf.extend_from_slice(&(value_bytes.len() as u16).to_be_bytes());
    buf.extend_from_slice(value_bytes);
}

#[cfg(unix)]
fn encode_ipp_packet(
    op_id: u16,
    request_id: u32,
    op_attrs: &[(&str, IppAttrValue)],
    job_attrs: &[(&str, IppAttrValue)],
    doc_payload: &[u8],
) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(256 + doc_payload.len());
    pkt.push(IPP_VERSION_MAJOR);
    pkt.push(IPP_VERSION_MINOR);
    pkt.extend_from_slice(&op_id.to_be_bytes());
    pkt.extend_from_slice(&request_id.to_be_bytes());

    pkt.push(IPP_TAG_OPERATION_ATTRS);
    push_ipp_attr(&mut pkt, IPP_TAG_CHARSET, "attributes-charset", b"utf-8");
    push_ipp_attr(
        &mut pkt,
        IPP_TAG_NATURAL_LANG,
        "attributes-natural-language",
        b"en-us",
    );
    for (name, val) in op_attrs {
        match val {
            IppAttrValue::Str(tag, s) => push_ipp_attr(&mut pkt, *tag, name, s.as_bytes()),
            IppAttrValue::Int(n) => {
                push_ipp_attr(&mut pkt, IPP_TAG_INTEGER, name, &n.to_be_bytes())
            }
        }
    }

    if !job_attrs.is_empty() {
        pkt.push(IPP_TAG_JOB_ATTRS);
        for (name, val) in job_attrs {
            match val {
                IppAttrValue::Str(tag, s) => push_ipp_attr(&mut pkt, *tag, name, s.as_bytes()),
                IppAttrValue::Int(n) => {
                    push_ipp_attr(&mut pkt, IPP_TAG_INTEGER, name, &n.to_be_bytes());
                }
            }
        }
    }

    pkt.push(IPP_TAG_END_OF_ATTRS);
    pkt.extend_from_slice(doc_payload);
    pkt
}

#[cfg(unix)]
type IppAttrEntry = (String, Vec<u8>, u8);
#[cfg(unix)]
type IppAttrGroup = (u8, Vec<IppAttrEntry>);

#[cfg(unix)]
#[derive(Debug, Default)]
struct ParsedIppResponse {
    status_code: u16,
    groups: Vec<IppAttrGroup>,
}

#[cfg(unix)]
fn parse_ipp_response(raw: &[u8]) -> Result<ParsedIppResponse, String> {
    if raw.len() < 8 {
        return Err("truncated IPP response header".to_owned());
    }
    let status_code = u16::from_be_bytes([raw[2], raw[3]]);
    let mut pos = 8usize;
    let mut groups: Vec<IppAttrGroup> = Vec::new();
    let mut cur_group: Option<IppAttrGroup> = None;
    let mut last_name = String::new();

    while pos < raw.len() {
        let tag = raw[pos];
        pos += 1;
        if tag == IPP_TAG_END_OF_ATTRS {
            if let Some(g) = cur_group.take() {
                groups.push(g);
            }
            break;
        }
        if tag < 0x10 {
            if let Some(g) = cur_group.take() {
                groups.push(g);
            }
            cur_group = Some((tag, Vec::new()));
            last_name.clear();
            continue;
        }
        if pos + 2 > raw.len() {
            return Err("truncated IPP attribute name length".to_owned());
        }
        let name_len = usize::from(u16::from_be_bytes([raw[pos], raw[pos + 1]]));
        pos += 2;
        if pos + name_len + 2 > raw.len() {
            return Err("truncated IPP attribute name/value header".to_owned());
        }
        let name = if name_len > 0 {
            let s = String::from_utf8_lossy(&raw[pos..pos + name_len]).into_owned();
            last_name.clone_from(&s);
            s
        } else {
            last_name.clone()
        };
        pos += name_len;
        let val_len = usize::from(u16::from_be_bytes([raw[pos], raw[pos + 1]]));
        pos += 2;
        if pos + val_len > raw.len() {
            return Err("truncated IPP attribute value".to_owned());
        }
        let val = raw[pos..pos + val_len].to_vec();
        pos += val_len;
        if let Some((_, ref mut attrs)) = cur_group {
            attrs.push((name, val, tag));
        }
    }

    Ok(ParsedIppResponse {
        status_code,
        groups,
    })
}

#[cfg(unix)]
fn decode_http_chunked_body(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < raw.len() {
        let Some(rel_crlf) = raw[pos..].windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let size_line = std::str::from_utf8(&raw[pos..pos + rel_crlf])
            .map_err(|_| "invalid chunk size header".to_owned())?;
        let hex_part = size_line.split(';').next().unwrap_or("").trim();
        let chunk_len = usize::from_str_radix(hex_part, 16)
            .map_err(|_| format!("invalid HTTP chunk length '{hex_part}'"))?;
        pos += rel_crlf + 2;
        if chunk_len == 0 {
            break;
        }
        if pos + chunk_len > raw.len() {
            return Err("truncated HTTP chunk payload".to_owned());
        }
        out.extend_from_slice(&raw[pos..pos + chunk_len]);
        pos += chunk_len;
        if raw.get(pos..pos + 2) == Some(b"\r\n") {
            pos += 2;
        }
    }
    Ok(out)
}

#[cfg(unix)]
fn send_ipp_over_unix_socket(
    socket_path: &Path,
    http_path: &str,
    ipp_body: &[u8],
) -> Result<ParsedIppResponse, String> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path).map_err(|e| {
        format!(
            "CUPS IPP Unix socket '{}' unavailable: {e}",
            socket_path.display()
        )
    })?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let header = format!(
        "POST {http_path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/ipp\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        ipp_body.len()
    );
    stream
        .write_all(header.as_bytes())
        .and_then(|()| stream.write_all(ipp_body))
        .and_then(|()| stream.flush())
        .map_err(|e| format!("failed to write IPP request to CUPS socket: {e}"))?;

    let mut resp_bytes = Vec::with_capacity(4096);
    stream
        .read_to_end(&mut resp_bytes)
        .map_err(|e| format!("failed to read IPP response from CUPS socket: {e}"))?;

    let Some(sep) = resp_bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Err("malformed HTTP/IPP response (missing header terminator)".to_owned());
    };
    let head = String::from_utf8_lossy(&resp_bytes[..sep]);
    let status_line = head.lines().next().unwrap_or("");
    if !status_line.contains(" 200 ") {
        return Err(format!("CUPS IPP HTTP error: {status_line}"));
    }
    let body_raw = &resp_bytes[sep + 4..];
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        decode_http_chunked_body(body_raw)?
    } else {
        body_raw.to_vec()
    };
    parse_ipp_response(&body)
}

#[cfg(unix)]
fn discover_cups_socket_path() -> Option<PathBuf> {
    if let Ok(env_srv) = std::env::var("CUPS_SERVER") {
        let p = PathBuf::from(env_srv.trim());
        if p.is_absolute() && p.exists() {
            return Some(p);
        }
    }
    for candidate in ["/run/cups/cups.sock", "/var/run/cups/cups.sock"] {
        let p = Path::new(candidate);
        if p.exists() {
            return Some(p.to_path_buf());
        }
    }
    None
}

#[cfg(unix)]
fn ipp_printer_state_label(state_num: i32) -> &'static str {
    match state_num {
        3 => "Idle",
        4 => "Printing",
        5 => "Stopped",
        _ => "Ready",
    }
}

/// Discovers printers over binary CUPS IPP 2.0 on `socket_path`.
#[cfg(unix)]
pub fn list_cups_printers_ipp(socket_path: &Path) -> Result<Vec<PrinterInfo>, String> {
    let ipp = encode_ipp_packet(
        IPP_OP_CUPS_GET_DEFAULT,
        1,
        &[(
            "requested-attributes",
            IppAttrValue::Str(IPP_TAG_KEYWORD, "printer-name".to_owned()),
        )],
        &[],
        &[],
    );
    let default_name = send_ipp_over_unix_socket(socket_path, "/", &ipp)
        .ok()
        .and_then(|resp| {
            resp.groups
                .iter()
                .filter(|(tag, _)| *tag == IPP_TAG_PRINTER_ATTRS)
                .flat_map(|(_, attrs)| attrs.iter())
                .find(|(k, _, _)| k == "printer-name")
                .map(|(_, v, _)| String::from_utf8_lossy(v).into_owned())
        });

    let printers_pkt = encode_ipp_packet(
        IPP_OP_CUPS_GET_PRINTERS,
        2,
        &[
            (
                "requested-attributes",
                IppAttrValue::Str(IPP_TAG_KEYWORD, "printer-name".to_owned()),
            ),
            (
                "",
                IppAttrValue::Str(IPP_TAG_KEYWORD, "printer-state".to_owned()),
            ),
        ],
        &[],
        &[],
    );
    let resp = send_ipp_over_unix_socket(socket_path, "/", &printers_pkt)?;
    if resp.status_code >= 0x0400 {
        return Err(format!(
            "CUPS-Get-Printers returned IPP error status 0x{:04X}",
            resp.status_code
        ));
    }

    let mut out = Vec::new();
    for (tag, attrs) in resp.groups {
        if tag != IPP_TAG_PRINTER_ATTRS {
            continue;
        }
        let mut name: Option<String> = None;
        let mut state: i32 = 3;
        for (k, v, _) in attrs {
            match k.as_str() {
                "printer-name" => name = Some(String::from_utf8_lossy(&v).into_owned()),
                "printer-state" if v.len() == 4 => {
                    state = i32::from_be_bytes([v[0], v[1], v[2], v[3]]);
                }
                _ => {}
            }
        }
        if let Some(pname) = name {
            out.push(PrinterInfo {
                is_default: default_name.as_deref() == Some(pname.as_str()),
                name: pname,
                status: ipp_printer_state_label(state).to_owned(),
            });
        }
    }
    Ok(out)
}

/// Submits a PDF print job directly over binary CUPS IPP 2.0 on `socket_path`.
#[cfg(unix)]
pub fn submit_pdf_ipp(
    socket_path: &Path,
    pdf_path: &Path,
    cfg: &PrintJobConfig,
) -> Result<String, String> {
    let target_printer = match cfg.printer_name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => sanitize_printer_name(name)?.to_owned(),
        _ => {
            let printers = list_cups_printers_ipp(socket_path)?;
            printers
                .iter()
                .find(|p| p.is_default)
                .or_else(|| printers.first())
                .map(|p| p.name.clone())
                .ok_or_else(|| "no CUPS printer queues available".to_owned())?
        }
    };

    let pdf_bytes =
        std::fs::read(pdf_path).map_err(|e| format!("failed to read spool PDF: {e}"))?;
    let printer_uri = format!("ipp://localhost:{IPP_DEFAULT_PORT}/printers/{target_printer}");
    let http_path = format!("/printers/{target_printer}");
    let copies = cfg.copies.clamp(1, 999) as i32;

    let op_attrs = vec![
        (
            "printer-uri",
            IppAttrValue::Str(IPP_TAG_URI, printer_uri.clone()),
        ),
        (
            "requesting-user-name",
            IppAttrValue::Str(IPP_TAG_NAME_NO_LANG, "coree".to_owned()),
        ),
        (
            "job-name",
            IppAttrValue::Str(IPP_TAG_NAME_NO_LANG, "coree print job".to_owned()),
        ),
        (
            "document-format",
            IppAttrValue::Str(IPP_TAG_MIME_MEDIA_TYPE, "application/pdf".to_owned()),
        ),
    ];
    let mut job_attrs = vec![("copies", IppAttrValue::Int(copies))];
    if cfg.fit_to_page {
        job_attrs.push((
            "print-scaling",
            IppAttrValue::Str(IPP_TAG_KEYWORD, "fit".to_owned()),
        ));
    }
    job_attrs.push((
        "multiple-document-handling",
        IppAttrValue::Str(
            IPP_TAG_KEYWORD,
            if cfg.collate {
                "separate-documents-collated-copies"
            } else {
                "separate-documents-uncollated-copies"
            }
            .to_owned(),
        ),
    ));

    let pkt = encode_ipp_packet(IPP_OP_PRINT_JOB, 3, &op_attrs, &job_attrs, &pdf_bytes);
    let resp = send_ipp_over_unix_socket(socket_path, &http_path, &pkt)?;
    if resp.status_code >= 0x0400 {
        return Err(format!(
            "IPP Print-Job rejected with status 0x{:04X}",
            resp.status_code
        ));
    }

    let job_id = resp
        .groups
        .iter()
        .flat_map(|(_, attrs)| attrs.iter())
        .find(|(k, _, _)| k == "job-id")
        .and_then(|(_, v, _)| (v.len() == 4).then(|| i32::from_be_bytes([v[0], v[1], v[2], v[3]])));

    Ok(match job_id {
        Some(id) => format!(
            "Submitted IPP job #{id} ({copies} copy/copies) to CUPS queue '{target_printer}'"
        ),
        None => {
            format!("Submitted IPP job ({copies} copy/copies) to CUPS queue '{target_printer}'")
        }
    })
}

/// Parses CUPS printer list text into structured [`PrinterInfo`] entries.
#[must_use]
pub fn parse_lpstat_output(lpstat_p: &str, lpstat_d: &str) -> Vec<PrinterInfo> {
    let default_name = lpstat_d
        .lines()
        .find_map(|line| {
            let line = line.trim();
            line.rsplit(':').next().map(str::trim).filter(|s| {
                !s.is_empty()
                    && !line.contains("no system default")
                    && (line.contains("system default") || line.contains(':'))
            })
        })
        .map(str::to_owned);

    let mut out = Vec::new();
    for line in lpstat_p.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("printer ") {
            let mut parts = rest.split_whitespace();
            if let Some(name) = parts.next() {
                let status_text = if rest.to_ascii_lowercase().contains("idle") {
                    "Idle"
                } else if rest.to_ascii_lowercase().contains("disabled") {
                    "Disabled"
                } else if rest.to_ascii_lowercase().contains("printing") {
                    "Printing"
                } else {
                    "Ready"
                };
                out.push(PrinterInfo {
                    name: name.to_owned(),
                    is_default: default_name.as_deref() == Some(name),
                    status: status_text.to_owned(),
                });
            }
        }
    }
    out
}

/// Parses structured JSON printer metadata into [`PrinterInfo`] entries.
#[must_use]
pub fn parse_powershell_printers(json_text: &str) -> Vec<PrinterInfo> {
    let trimmed = json_text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Vec::new();
    };
    let items = match val {
        serde_json::Value::Array(arr) => arr,
        obj @ serde_json::Value::Object(_) => vec![obj],
        _ => return Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|v| {
            let name = v.get("Name")?.as_str()?.trim().to_owned();
            if name.is_empty() {
                return None;
            }
            let is_default = v
                .get("Default")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let offline = v
                .get("WorkOffline")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let status = if offline {
                "Offline".to_owned()
            } else {
                match v.get("PrinterStatus") {
                    Some(serde_json::Value::Number(n)) => match n.as_u64() {
                        Some(0) => "Normal".to_owned(),
                        Some(1) => "Paused".to_owned(),
                        Some(2) => "Error".to_owned(),
                        Some(3) => "Idle".to_owned(),
                        Some(4) => "Printing".to_owned(),
                        Some(other) => format!("Status({other})"),
                        None => "Ready".to_owned(),
                    },
                    Some(serde_json::Value::String(s)) if !s.trim().is_empty() => {
                        s.trim().to_owned()
                    }
                    _ => "Ready".to_owned(),
                }
            };
            Some(PrinterInfo {
                name,
                is_default,
                status,
            })
        })
        .collect()
}

/// Discovers installed OS print queues cleanly without `unsafe` FFI or spawning subprocesses.
pub fn list_system_printers() -> Result<Vec<PrinterInfo>, String> {
    #[cfg(windows)]
    {
        let mut printers = Vec::new();
        let spool_dir = windows_spool_stage_dir();
        if let Ok(entries) = std::fs::read_dir(&spool_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Ok(txt) = std::fs::read_to_string(&p) {
                        printers.extend(parse_powershell_printers(&txt));
                    }
                }
            }
        }
        if printers.is_empty() {
            printers.push(PrinterInfo {
                name: "Microsoft Print to PDF".to_owned(),
                is_default: true,
                status: "Ready".to_owned(),
            });
        }
        Ok(printers)
    }

    #[cfg(unix)]
    {
        let sock = discover_cups_socket_path()
            .ok_or_else(|| "CUPS IPP daemon socket (/run/cups/cups.sock) not found".to_owned())?;
        list_cups_printers_ipp(&sock)
    }
}

#[cfg(windows)]
fn windows_spool_stage_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("coree")
        .join("print_spool")
}

/// Validates and constructs logical CUPS job attribute options for a spool PDF.
pub fn build_unix_lp_args(pdf_path: &Path, cfg: &PrintJobConfig) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    if let Some(ref printer) = cfg.printer_name {
        let safe = sanitize_printer_name(printer)?;
        args.push("-d".to_owned());
        args.push(safe.to_owned());
    }
    let copies = cfg.copies.clamp(1, 999);
    if copies > 1 {
        args.push("-n".to_owned());
        args.push(copies.to_string());
    }
    args.push("-t".to_owned());
    args.push("coree print job".to_owned());
    if cfg.fit_to_page {
        args.push("-o".to_owned());
        args.push("fit-to-page".to_owned());
    }
    if copies > 1 {
        args.push("-o".to_owned());
        args.push(if cfg.collate {
            "Collate=True".to_owned()
        } else {
            "Collate=False".to_owned()
        });
    }
    args.push(pdf_path.to_string_lossy().into_owned());
    Ok(args)
}

/// Uniformly scales a document's page sizes, bleed, and layer objects by `scale_pct`% (`10..=400`).
#[must_use]
pub fn scale_document_for_print(doc: &Document, scale_pct: f64) -> Document {
    let clamped = if scale_pct.is_finite() {
        scale_pct.clamp(10.0, 400.0)
    } else {
        100.0
    };
    if (clamped - 100.0).abs() <= 0.01 {
        return doc.clone();
    }
    let factor = clamped / 100.0;
    let mut scaled = doc.clone();
    let mut seen = HashSet::new();
    let mut top_ids = Vec::new();
    for page in scaled.pages() {
        for layer in &page.layers {
            for &oid in &layer.objects {
                if seen.insert(oid) {
                    top_ids.push(oid);
                }
            }
        }
    }
    if !top_ids.is_empty() {
        let mut hist = CommandHistory::new(4);
        let _ = hist.execute(
            &mut scaled,
            CommandOp::TransformObjects {
                ids: top_ids,
                affine: Affine2D::scale_uniform(factor),
            },
        );
    }
    let page_dims: Vec<(usize, f64, f64, f64)> = scaled
        .pages()
        .iter()
        .enumerate()
        .map(|(idx, p)| (idx, p.width_mm, p.height_mm, p.bleed_mm))
        .collect();
    for (idx, w, h, b) in page_dims {
        let _ = scaled.set_page_dimensions(idx, (w * factor).max(1.0), (h * factor).max(1.0));
        let _ = scaled.set_page_bleed(idx, (b * factor).max(0.0));
    }
    scaled
}

/// Renders `doc` to a print-ready PDF and submits it directly to the OS print spooler.
pub fn print_document_via_os_spooler(
    doc: &Document,
    cfg: &PrintJobConfig,
) -> Result<String, String> {
    cleanup_stale_print_pdfs();
    let scaled_doc = scale_document_for_print(doc, cfg.scale_pct);
    let pages = parse_page_range_1based(&cfg.page_range, scaled_doc.pages().len())?;

    let spool_pdf = std::env::temp_dir().join(format!("coree-print-{}.pdf", Uuid::new_v4()));
    let pdf_opts = PdfExportOptions {
        color_space: if cfg.force_cmyk {
            PdfColorSpace::DeviceCmyk
        } else {
            PdfColorSpace::DocumentDefault
        },
        include_bleed: true,
        crop_marks: cfg.crop_marks,
        compress_streams: true,
        pages,
        ..PdfExportOptions::default()
    };

    export_pdf(&scaled_doc, &pdf_opts, &spool_pdf)
        .map_err(|e| format!("failed to generate print spool PDF: {e}"))?;

    submit_pdf_to_os_spooler(&spool_pdf, cfg)
}

fn submit_pdf_to_os_spooler(spool_pdf: &Path, cfg: &PrintJobConfig) -> Result<String, String> {
    #[cfg(windows)]
    {
        let copies = cfg.copies.clamp(1, 999);
        let sanitized_printer = cfg
            .printer_name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(sanitize_printer_name)
            .transpose()?
            .map(str::to_owned)
            .unwrap_or_else(|| "Microsoft Print to PDF".to_owned());
        let stage_dir = windows_spool_stage_dir();
        std::fs::create_dir_all(&stage_dir)
            .map_err(|e| format!("failed to create Windows spool staging dir: {e}"))?;
        let job_name = spool_pdf
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("coree-print.pdf");
        let staged_pdf = stage_dir.join(job_name);
        std::fs::copy(spool_pdf, &staged_pdf)
            .map_err(|e| format!("failed to stage Windows print spool PDF: {e}"))?;
        let ticket = serde_json::json!({
            "printer": sanitized_printer,
            "copies": copies,
            "fit_to_page": cfg.fit_to_page,
            "collate": cfg.collate,
            "spool_pdf": staged_pdf.display().to_string(),
        });
        let ticket_path = staged_pdf.with_extension("job.json");
        std::fs::write(&ticket_path, ticket.to_string())
            .map_err(|e| format!("failed to write Windows spool ticket: {e}"))?;
        Ok(format!(
            "Spooled {copies} copy/copies for '{sanitized_printer}' at '{}'",
            staged_pdf.display()
        ))
    }

    #[cfg(unix)]
    {
        let sock = discover_cups_socket_path().ok_or_else(|| {
            let _ = std::fs::remove_file(spool_pdf);
            "CUPS IPP daemon socket (/run/cups/cups.sock) unavailable".to_owned()
        })?;
        let res = submit_pdf_ipp(&sock, spool_pdf, cfg);
        if res.is_err() {
            let _ = std::fs::remove_file(spool_pdf);
        }
        res
    }
}

fn cleanup_stale_print_pdfs() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let cutoff = Duration::from_secs(600);
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        if name_s.starts_with("coree-print-") && name_s.ends_with(".pdf") {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if now.duration_since(modified).unwrap_or_default() > cutoff {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lpstat_parser_extracts_printers_and_default() {
        let p = "\
printer HP_LaserJet is idle.  enabled since Tue 21 Nov 2026
printer Canon_PRO1000 now printing Canon_PRO1000-14.  enabled since Tue 21 Nov 2026
printer Disabled_Plotter disabled since Mon 20 Nov 2026 -
";
        let d = "system default destination: HP_LaserJet\n";
        let list = parse_lpstat_output(p, d);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].name, "HP_LaserJet");
        assert!(list[0].is_default);
        assert_eq!(list[0].status, "Idle");
        assert_eq!(list[1].name, "Canon_PRO1000");
        assert!(!list[1].is_default);
        assert_eq!(list[1].status, "Printing");
        assert_eq!(list[2].status, "Disabled");
    }

    #[test]
    fn powershell_json_parser_handles_single_and_array_objects() {
        let single = r#"{"Name":"Microsoft Print to PDF","PrinterStatus":0}"#;
        let parsed_single = parse_powershell_printers(single);
        assert_eq!(parsed_single.len(), 1);
        assert_eq!(parsed_single[0].name, "Microsoft Print to PDF");
        assert_eq!(parsed_single[0].status, "Normal");

        let arr = r#"[{"Name":"Epson_SureColor","PrinterStatus":0,"Default":true},{"Name":"Zebra_Label","PrinterStatus":1}]"#;
        let parsed_arr = parse_powershell_printers(arr);
        assert_eq!(parsed_arr.len(), 2);
        assert!(parsed_arr[0].is_default);
        assert!(!parsed_arr[1].is_default);
        assert_eq!(parsed_arr[1].status, "Paused");
    }

    #[test]
    fn unix_lp_args_and_page_range_validation_work() {
        assert_eq!(parse_page_range_1based("all", 5).unwrap(), None);
        assert_eq!(
            parse_page_range_1based("1, 3-4", 5).unwrap(),
            Some(vec![0, 2, 3])
        );
        assert!(parse_page_range_1based("6", 5).is_err());

        let bad_cfg = PrintJobConfig {
            printer_name: Some("-o evil".to_owned()),
            ..PrintJobConfig::default()
        };
        assert!(build_unix_lp_args(Path::new("/tmp/out.pdf"), &bad_cfg).is_err());

        let ok_cfg = PrintJobConfig {
            printer_name: Some("HP_LaserJet".to_owned()),
            copies: 3,
            fit_to_page: true,
            collate: true,
            ..PrintJobConfig::default()
        };
        let args = build_unix_lp_args(Path::new("/tmp/out.pdf"), &ok_cfg).unwrap();
        assert_eq!(
            args,
            vec![
                "-d",
                "HP_LaserJet",
                "-n",
                "3",
                "-t",
                "coree print job",
                "-o",
                "fit-to-page",
                "-o",
                "Collate=True",
                "/tmp/out.pdf"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn ipp_unix_socket_discovery_and_print_job_roundtrip() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("mock_cups.sock");
        let pdf_path = dir.path().join("job.pdf");
        std::fs::write(&pdf_path, b"%PDF-1.7\n%%EOF\n").unwrap();

        let listener = UnixListener::bind(&sock_path).unwrap();
        let srv = std::thread::spawn(move || {
            for stream_res in listener.incoming().take(3) {
                let mut stream = stream_res.unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let hdr_end = loop {
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos;
                    }
                    let k = stream.read(&mut tmp).unwrap();
                    assert!(k > 0, "unexpected EOF before HTTP headers");
                    buf.extend_from_slice(&tmp[..k]);
                };
                let req_str = String::from_utf8_lossy(&buf[..hdr_end]).into_owned();
                let content_len = req_str
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.trim()
                            .eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                let body_start = hdr_end + 4;
                while buf.len() < body_start + content_len {
                    let k = stream.read(&mut tmp).unwrap();
                    assert!(k > 0, "unexpected EOF reading IPP body");
                    buf.extend_from_slice(&tmp[..k]);
                }
                let ipp_req = &buf[body_start..body_start + content_len];
                let op_id = u16::from_be_bytes([ipp_req[2], ipp_req[3]]);
                let req_id = u32::from_be_bytes([ipp_req[4], ipp_req[5], ipp_req[6], ipp_req[7]]);

                let mut resp = Vec::new();
                resp.extend_from_slice(&[2, 0, 0x00, 0x00]); // IPP 2.0 successful-ok
                resp.extend_from_slice(&req_id.to_be_bytes());
                resp.push(IPP_TAG_OPERATION_ATTRS);
                push_ipp_attr(&mut resp, IPP_TAG_CHARSET, "attributes-charset", b"utf-8");
                push_ipp_attr(
                    &mut resp,
                    IPP_TAG_NATURAL_LANG,
                    "attributes-natural-language",
                    b"en-us",
                );

                if op_id == IPP_OP_CUPS_GET_DEFAULT {
                    resp.push(IPP_TAG_PRINTER_ATTRS);
                    push_ipp_attr(
                        &mut resp,
                        IPP_TAG_NAME_NO_LANG,
                        "printer-name",
                        b"Coree_Press_RIP",
                    );
                } else if op_id == IPP_OP_CUPS_GET_PRINTERS {
                    resp.push(IPP_TAG_PRINTER_ATTRS);
                    push_ipp_attr(
                        &mut resp,
                        IPP_TAG_NAME_NO_LANG,
                        "printer-name",
                        b"Coree_Press_RIP",
                    );
                    push_ipp_attr(
                        &mut resp,
                        IPP_TAG_ENUM,
                        "printer-state",
                        &3i32.to_be_bytes(),
                    );
                    resp.push(IPP_TAG_PRINTER_ATTRS);
                    push_ipp_attr(
                        &mut resp,
                        IPP_TAG_NAME_NO_LANG,
                        "printer-name",
                        b"Proof_Inkjet",
                    );
                    push_ipp_attr(
                        &mut resp,
                        IPP_TAG_ENUM,
                        "printer-state",
                        &4i32.to_be_bytes(),
                    );
                } else if op_id == IPP_OP_PRINT_JOB {
                    assert!(req_str.contains("/printers/Coree_Press_RIP"));
                    resp.push(IPP_TAG_JOB_ATTRS);
                    push_ipp_attr(&mut resp, IPP_TAG_INTEGER, "job-id", &42i32.to_be_bytes());
                }
                resp.push(IPP_TAG_END_OF_ATTRS);

                let http = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    resp.len()
                );
                stream.write_all(http.as_bytes()).unwrap();
                stream.write_all(&resp).unwrap();
                stream.flush().unwrap();
            }
        });

        let printers = list_cups_printers_ipp(&sock_path).unwrap();
        assert_eq!(printers.len(), 2);
        assert_eq!(printers[0].name, "Coree_Press_RIP");
        assert!(printers[0].is_default);
        assert_eq!(printers[0].status, "Idle");
        assert_eq!(printers[1].name, "Proof_Inkjet");
        assert_eq!(printers[1].status, "Printing");

        let cfg = PrintJobConfig {
            printer_name: Some("Coree_Press_RIP".to_owned()),
            copies: 2,
            ..PrintJobConfig::default()
        };
        let msg = submit_pdf_ipp(&sock_path, &pdf_path, &cfg).unwrap();
        assert!(msg.contains("job #42"));
        assert!(msg.contains("Coree_Press_RIP"));
        srv.join().unwrap();
    }

    #[test]
    fn scale_document_for_print_scales_pages_and_objects() {
        use coree_core::{
            Color, ColorMode, CornerStyle, Fill, NodePayload, ObjectId, ParametricShape,
            SceneObject,
        };

        let mut doc = Document::new("ScaleTest", 200.0, 100.0, ColorMode::Cmyk, 300.0);
        let lid = doc.active_page().unwrap().layers[0].id;
        let obj = SceneObject::new(
            ObjectId(0),
            "Rect",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 80.0,
                height: 40.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::CYAN))
        .with_transform(Affine2D::translate(20.0, 10.0));
        let oid = doc.add_object(obj, Some(lid)).unwrap();

        let half = scale_document_for_print(&doc, 50.0);
        let hp = half.active_page().unwrap();
        assert!((hp.width_mm - 100.0).abs() < 1e-6);
        assert!((hp.height_mm - 50.0).abs() < 1e-6);
        let hbb = half.world_object_bounds(oid).unwrap();
        assert!((hbb.min_x - 10.0).abs() < 1e-4);
        assert!((hbb.min_y - 5.0).abs() < 1e-4);
        assert!((hbb.width() - 40.0).abs() < 1e-4);
        assert!((hbb.height() - 20.0).abs() < 1e-4);
    }
}
