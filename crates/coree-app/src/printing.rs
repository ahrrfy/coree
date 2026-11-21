// SPDX-License-Identifier: MIT OR Apache-2.0

//! Zero-`unsafe`, cross-platform native OS printer discovery and spooler submission.
//!
//! Renders vector print jobs cleanly to PDF/X-compatible PDF via `coree_io::export_pdf`
//! and submits to CUPS (`lpstat`/`lp`/`lpr`) on Linux/macOS or Windows Spooler
//! (`Get-Printer` + `Start-Process -Verb PrintTo` / `-Verb Print`) on Windows.

use coree_core::Document;
use coree_io::{export_pdf, PdfColorSpace, PdfExportOptions};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// Discovered local or network OS printer queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterInfo {
    pub name: String,
    pub is_default: bool,
    pub status: String,
}

/// Native OS print job configuration shown in Print Studio (`Ctrl+P`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintJobConfig {
    pub printer_name: Option<String>,
    pub copies: u32,
    /// `"all"` or 1-based page specification like `"1,3-5"`.
    pub page_range: String,
    pub fit_to_page: bool,
    pub force_cmyk: bool,
    pub crop_marks: bool,
    pub collate: bool,
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
    if trimmed.chars().any(|c| c.is_control() || c == '\0') {
        return Err("invalid printer name: control characters rejected".to_owned());
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

/// Parses CUPS `lpstat -p` + `lpstat -d` output into structured printer entries.
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

/// Parses JSON produced by PowerShell `Get-Printer | Select-Object Name,PrinterStatus | ConvertTo-Json`.
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
            let status = match v.get("PrinterStatus") {
                Some(serde_json::Value::Number(n)) => match n.as_u64() {
                    Some(0) => "Normal".to_owned(),
                    Some(1) => "Paused".to_owned(),
                    Some(2) => "Error".to_owned(),
                    Some(other) => format!("Status({other})"),
                    None => "Ready".to_owned(),
                },
                Some(serde_json::Value::String(s)) if !s.trim().is_empty() => s.trim().to_owned(),
                _ => "Ready".to_owned(),
            };
            Some(PrinterInfo {
                name,
                is_default: false,
                status,
            })
        })
        .collect()
}

/// Queries the operating system for installed print queues without unsafe FFI.
pub fn list_system_printers() -> Result<Vec<PrinterInfo>, String> {
    #[cfg(target_os = "windows")]
    {
        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-Printer | Select-Object Name,PrinterStatus | ConvertTo-Json -Compress",
            ])
            .output()
            .map_err(|e| format!("failed to query Windows printers: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            return Err(if err.is_empty() {
                "Windows printer query returned non-zero status".to_owned()
            } else {
                err
            });
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut printers = parse_powershell_printers(&stdout);
        // Query default printer name cleanly if available.
        if let Ok(def_out) = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "(Get-CimInstance Win32_Printer -Filter 'Default = True' | Select-Object -First 1 -ExpandProperty Name)",
            ])
            .output()
        {
            let def_name = String::from_utf8_lossy(&def_out.stdout).trim().to_owned();
            if !def_name.is_empty() {
                for p in &mut printers {
                    if p.name == def_name {
                        p.is_default = true;
                    }
                }
            }
        }
        Ok(printers)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let lpstat_p = Command::new("lpstat")
            .arg("-p")
            .output()
            .map_err(|e| format!("CUPS spooler (`lpstat`) unavailable: {e}"))?;
        let lpstat_d = Command::new("lpstat")
            .arg("-d")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let stdout = String::from_utf8_lossy(&lpstat_p.stdout);
        Ok(parse_lpstat_output(&stdout, &lpstat_d))
    }
}

/// Builds validated CUPS `lp` command arguments for a generated spool PDF.
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

/// Renders `doc` to a print-ready PDF and submits it to the OS print spooler.
pub fn print_document_via_os_spooler(
    doc: &Document,
    cfg: &PrintJobConfig,
) -> Result<String, String> {
    cleanup_stale_print_pdfs();
    let pages = parse_page_range_1based(&cfg.page_range, doc.pages.len())?;

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

    export_pdf(doc, &pdf_opts, &spool_pdf)
        .map_err(|e| format!("failed to generate print spool PDF: {e}"))?;

    submit_pdf_to_os_spooler(&spool_pdf, cfg)
}

fn submit_pdf_to_os_spooler(spool_pdf: &Path, cfg: &PrintJobConfig) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        let copies = cfg.copies.clamp(1, 999);
        let target_label = cfg
            .printer_name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("default Windows printer");

        let mut script_ok = true;
        let ps_cmd = if let Some(ref printer) = cfg.printer_name {
            let safe = sanitize_printer_name(printer)?;
            let _ = safe;
            "Start-Process -FilePath $env:COREE_PRINT_PDF -Verb PrintTo -ArgumentList $env:COREE_PRINTER"
        } else {
            "Start-Process -FilePath $env:COREE_PRINT_PDF -Verb Print"
        };

        for _ in 0..copies {
            let mut cmd = Command::new("powershell");
            cmd.args(["-NoProfile", "-NonInteractive", "-Command", ps_cmd]);
            cmd.env("COREE_PRINT_PDF", spool_pdf);
            if let Some(ref printer) = cfg.printer_name {
                cmd.env("COREE_PRINTER", sanitize_printer_name(printer)?);
            }
            let out = cmd
                .output()
                .map_err(|e| format!("failed to invoke Windows spooler: {e}"))?;
            if !out.status.success() {
                script_ok = false;
                let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
                return Err(if err.is_empty() {
                    "Windows print verb failed (ensure a PDF handler or printer is configured)"
                        .to_owned()
                } else {
                    err
                });
            }
        }
        let _ = script_ok;
        Ok(format!(
            "Submitted {copies} copy/copies of '{}' to {target_label}",
            spool_pdf.display()
        ))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let args = build_unix_lp_args(spool_pdf, cfg)?;
        match Command::new("lp").args(&args).output() {
            Ok(out) => {
                if !out.status.success() {
                    let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
                    let _ = std::fs::remove_file(spool_pdf);
                    return Err(if err.is_empty() {
                        "CUPS `lp` rejected print job".to_owned()
                    } else {
                        err
                    });
                }
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                Ok(if stdout.is_empty() {
                    "Submitted print job to CUPS `lp`".to_owned()
                } else {
                    stdout
                })
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // Fallback to BSD `lpr` when `lp` is absent.
                let mut lpr_args = Vec::new();
                if let Some(ref printer) = cfg.printer_name {
                    let safe = sanitize_printer_name(printer)?;
                    lpr_args.push("-P".to_owned());
                    lpr_args.push(safe.to_owned());
                }
                let copies = cfg.copies.clamp(1, 999);
                if copies > 1 {
                    lpr_args.push(format!("-#{copies}"));
                }
                lpr_args.push(spool_pdf.to_string_lossy().into_owned());
                let out = Command::new("lpr")
                    .args(&lpr_args)
                    .output()
                    .map_err(|e| format!("neither `lp` nor `lpr` is available: {e}"))?;
                if !out.status.success() {
                    let msg = String::from_utf8_lossy(&out.stderr).trim().to_owned();
                    let _ = std::fs::remove_file(spool_pdf);
                    return Err(if msg.is_empty() {
                        "`lpr` rejected print job".to_owned()
                    } else {
                        msg
                    });
                }
                Ok("Submitted print job to `lpr`".to_owned())
            }
            Err(err) => {
                let _ = std::fs::remove_file(spool_pdf);
                Err(format!("failed to invoke `lp`: {err}"))
            }
        }
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

        let arr = r#"[{"Name":"Epson_SureColor","PrinterStatus":0},{"Name":"Zebra_Label","PrinterStatus":1}]"#;
        let parsed_arr = parse_powershell_printers(arr);
        assert_eq!(parsed_arr.len(), 2);
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
}
