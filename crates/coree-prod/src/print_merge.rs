// SPDX-License-Identifier: MIT OR Apache-2.0

//! Variable Data / Print Merge engine:
//! - RFC-4180 compliant CSV and TSV parser (quoted commas, escaped quotes, embedded newlines,
//!   full UTF-8 / Arabic RTL support)
//! - Automatic padded serial numbering (`0001..0100`, configurable prefix/suffix/step)
//! - `<<Placeholder>>` substitution across text objects, tables, and dynamic vector barcode/QR
//!   objects, generating one merged page per data record.

use crate::barcode::{
    add_barcode_to_document, add_qr_code_to_document, BarcodeOptions, BarcodeSymbology,
    QrCodeOptions,
};
use crate::error::{ProdError, ProdResult};
use crate::scene_helpers::{clone_document_shell, clone_page_contents_into, get_page};
use coree_core::{Affine2D, Document, LayerId, NodePayload, ObjectId, Point2D};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Configuration for generating automatic padded serial numbers during print merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialNumberConfig {
    /// Placeholder column name (e.g. `"Serial"` -> `<<Serial>>`).
    pub column_name: String,
    /// Starting number (e.g. `1`).
    pub start: u64,
    /// Step increment per row (default `1`).
    pub step: u64,
    /// Zero-padding width (e.g. `4` produces `"0001"` .. `"0100"`).
    pub pad_width: usize,
    /// Optional prefix before the padded number (e.g. `"INV-"`).
    pub prefix: String,
    /// Optional suffix after the padded number.
    pub suffix: String,
}

impl Default for SerialNumberConfig {
    fn default() -> Self {
        Self {
            column_name: "Serial".to_owned(),
            start: 1,
            step: 1,
            pad_width: 4,
            prefix: String::new(),
            suffix: String::new(),
        }
    }
}

impl SerialNumberConfig {
    /// Formats the serial number for 0-based row index `row_idx`.
    #[must_use]
    pub fn format_row(&self, row_idx: usize) -> String {
        let val = self
            .start
            .saturating_add((row_idx as u64).saturating_mul(self.step.max(1)));
        let capped_width = self.pad_width.min(64);
        if capped_width > 0 {
            format!(
                "{}{:0width$}{}",
                self.prefix,
                val,
                self.suffix,
                width = capped_width
            )
        } else {
            format!("{}{val}{}", self.prefix, self.suffix)
        }
    }
}

/// Tabular variable-data source for [`apply_print_merge`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PrintMergeTable {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl PrintMergeTable {
    /// Parses an RFC-4180 CSV string (comma-delimited, UTF-8 / Arabic safe).
    pub fn from_csv(input: &str) -> ProdResult<Self> {
        Self::from_delimited(input, ',')
    }

    /// Parses a tab-separated (TSV) string with full RFC-4180 quoting support.
    pub fn from_tsv(input: &str) -> ProdResult<Self> {
        Self::from_delimited(input, '\t')
    }

    /// Parses a delimited table supporting RFC-4180 quotes, `""` escaping, embedded newlines,
    /// `\r\n`/`\n` line endings, and UTF-8 BOM stripping.
    pub fn from_delimited(input: &str, delimiter: char) -> ProdResult<Self> {
        if matches!(delimiter, '"' | '\r' | '\n') {
            return Err(ProdError::CsvParseError {
                line: 1,
                reason: format!("invalid CSV/TSV delimiter {delimiter:?}"),
            });
        }
        let text = input.strip_prefix('\u{FEFF}').unwrap_or(input);
        let records = parse_rfc4180(text, delimiter)?;
        if records.is_empty() {
            return Err(ProdError::CsvParseError {
                line: 1,
                reason: "input contains no header row".into(),
            });
        }
        let headers: Vec<String> = records[0].iter().map(|h| h.trim().to_owned()).collect();
        if headers.is_empty() || headers.iter().all(String::is_empty) {
            return Err(ProdError::CsvParseError {
                line: 1,
                reason: "header row is empty".into(),
            });
        }

        let col_count = headers.len();
        let mut rows = Vec::with_capacity(records.len().saturating_sub(1));
        for mut rec in records.into_iter().skip(1) {
            // Skip completely blank trailing lines
            if rec.len() == 1 && rec[0].is_empty() && col_count > 1 {
                continue;
            }
            rec.resize(col_count, String::new());
            rows.push(rec);
        }

        Ok(Self { headers, rows })
    }

    /// Creates a table containing solely an auto-numbered serial column of `count` records.
    #[must_use]
    pub fn from_serial_range(config: &SerialNumberConfig, count: usize) -> Self {
        let headers = vec![config.column_name.clone()];
        let rows = (0..count).map(|i| vec![config.format_row(i)]).collect();
        Self { headers, rows }
    }

    /// Adds (or replaces) a generated serial-number column across all existing rows.
    pub fn add_serial_column(&mut self, config: &SerialNumberConfig) {
        if let Some(pos) = self
            .headers
            .iter()
            .position(|h| h.eq_ignore_ascii_case(&config.column_name))
        {
            for (idx, row) in self.rows.iter_mut().enumerate() {
                if pos < row.len() {
                    row[pos] = config.format_row(idx);
                }
            }
        } else {
            self.headers.push(config.column_name.clone());
            for (idx, row) in self.rows.iter_mut().enumerate() {
                row.push(config.format_row(idx));
            }
        }
    }

    /// Number of data records (excluding the header row).
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Builds a field-name -> value map for `row_idx`.
    #[must_use]
    pub fn row_map(&self, row_idx: usize) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        if let Some(row) = self.rows.get(row_idx) {
            for (h, val) in self.headers.iter().zip(row.iter()) {
                map.insert(h.clone(), val.clone());
            }
        }
        map
    }
}

/// Pure-Rust RFC-4180 state-machine parser handling quoted delimiters, doubled quotes, and
/// multiline quoted fields cleanly.
fn parse_rfc4180(input: &str, delimiter: char) -> ProdResult<Vec<Vec<String>>> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_field = String::new();
    let mut in_quotes = false;
    let mut field_started = false;
    let mut line = 1usize;

    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    current_field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                if ch == '\n' {
                    line += 1;
                }
                current_field.push(ch);
            }
        } else if ch == '"' && !field_started {
            in_quotes = true;
            field_started = true;
        } else if ch == delimiter {
            current_row.push(std::mem::take(&mut current_field));
            field_started = false;
        } else if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            current_row.push(std::mem::take(&mut current_field));
            records.push(std::mem::take(&mut current_row));
            field_started = false;
            line += 1;
        } else if ch == '\n' {
            current_row.push(std::mem::take(&mut current_field));
            records.push(std::mem::take(&mut current_row));
            field_started = false;
            line += 1;
        } else {
            field_started = true;
            current_field.push(ch);
        }
    }

    if in_quotes {
        return Err(ProdError::CsvParseError {
            line,
            reason: "unterminated quoted field".into(),
        });
    }

    if field_started || !current_row.is_empty() {
        current_row.push(current_field);
        records.push(current_row);
    }

    Ok(records)
}

/// Configuration for [`apply_print_merge`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintMergeOptions {
    /// Zero-based index of the template page in `template_doc` (default: `0`).
    pub template_page_index: usize,
    /// Optional automatic serial-number column injected per record if not already present.
    pub serial: Option<SerialNumberConfig>,
    /// Default 1D barcode symbology when a barcode placeholder does not name one explicitly.
    pub default_barcode_symbology: BarcodeSymbology,
    /// Vector styling options for regenerated 1D barcodes.
    pub barcode_options: BarcodeOptions,
    /// Vector styling options for regenerated QR codes.
    pub qr_options: QrCodeOptions,
    /// Output page name pattern supporting `<<Field>>` and built-in `<<_Row>>` placeholders.
    pub page_name_pattern: String,
}

impl Default for PrintMergeOptions {
    fn default() -> Self {
        Self {
            template_page_index: 0,
            serial: None,
            default_barcode_symbology: BarcodeSymbology::Code128,
            barcode_options: BarcodeOptions::default(),
            qr_options: QrCodeOptions::default(),
            page_name_pattern: "Record <<_Row>>".to_owned(),
        }
    }
}

/// Generates a multi-page merged [`Document`] by cloning `template_doc.pages[options.template_page_index]`
/// once per row in `table` and substituting `<<Field>>` placeholders across text, table cells,
/// and barcode/QR template objects.
pub fn apply_print_merge(
    template_doc: &Document,
    table: &PrintMergeTable,
    options: &PrintMergeOptions,
) -> ProdResult<Document> {
    let tpl_page = get_page(template_doc, options.template_page_index)?;
    if table.rows.is_empty() {
        return Err(ProdError::MergeError(
            "print merge table contains zero data rows".into(),
        ));
    }

    let tpl_w = tpl_page.width_mm;
    let tpl_h = tpl_page.height_mm;
    let tpl_bleed = if tpl_page.bleed_mm.is_finite() {
        tpl_page.bleed_mm.max(0.0)
    } else {
        0.0
    };
    if !tpl_w.is_finite() || tpl_w <= 0.0 || !tpl_h.is_finite() || tpl_h <= 0.0 {
        return Err(ProdError::MergeError(
            "template page dimensions must be positive and finite".into(),
        ));
    }

    let mut out = clone_document_shell(template_doc, format!("{} (Merged)", template_doc.title()));

    for row_idx in 0..table.rows.len() {
        let mut fields = table.row_map(row_idx);
        fields.insert("_Row".to_owned(), (row_idx + 1).to_string());
        if let Some(serial_cfg) = &options.serial {
            fields
                .entry(serial_cfg.column_name.clone())
                .or_insert_with(|| serial_cfg.format_row(row_idx));
        }

        let page_name = substitute_placeholders(&options.page_name_pattern, &fields);
        out.add_page(page_name, tpl_w, tpl_h, tpl_bleed);
        let dst_page_idx = out.pages().len() - 1;

        let top_ids = clone_page_contents_into(
            template_doc,
            options.template_page_index,
            &mut out,
            dst_page_idx,
            Affine2D::IDENTITY,
            None,
        )?;

        // Handle dynamic barcode/QR placeholder objects recursively (top-level + inside Groups/PowerClips)
        let page_layer_objects: Vec<(LayerId, Vec<ObjectId>)> = out.pages()[dst_page_idx]
            .layers
            .iter()
            .map(|l| (l.id, l.objects.clone()))
            .collect();

        let mut replacements: Vec<BarcodeReplacement> = Vec::new();
        for (lid, obj_ids) in &page_layer_objects {
            for &oid in obj_ids {
                collect_barcode_replacements(
                    &out,
                    oid,
                    BarcodeParent::Layer(*lid),
                    *lid,
                    &fields,
                    options,
                    0,
                    &mut replacements,
                );
            }
        }

        for rep in replacements {
            let new_id = match rep.directive {
                BarcodeDirective::Qr(data) => add_qr_code_to_document(
                    &mut out,
                    &data,
                    rep.origin,
                    &options.qr_options,
                    Some(rep.host_layer_id),
                )?,
                BarcodeDirective::Linear { symbology, data } => add_barcode_to_document(
                    &mut out,
                    symbology,
                    &data,
                    rep.origin,
                    &options.barcode_options,
                    Some(rep.host_layer_id),
                )?,
            };
            // Clean relational references (`Connector`, `Blend`, `TextOnPath`, etc.) while
            // `rep.old_id` is still attached to its parent container/layer so world-space
            // fallback anchors preserve ancestor transforms.
            out.cleanup_dangling_refs_for_object_ids(&[rep.old_id]);
            // Splice `new_id` cleanly into the exact parent container or layer slot of `old_id`
            match rep.parent {
                BarcodeParent::Layer(lid) => {
                    if let Some(layer) = out.find_layer_mut(lid) {
                        layer.objects.retain(|&id| id != new_id);
                        if let Some(pos) = layer.objects.iter().position(|&id| id == rep.old_id) {
                            layer.objects[pos] = new_id;
                        } else {
                            layer.objects.push(new_id);
                        }
                    }
                }
                BarcodeParent::Container(parent_id) => {
                    // Detach freshly added group from top-level layer and place inside parent container
                    if let Some(layer) = out.find_layer_mut(rep.host_layer_id) {
                        layer.objects.retain(|&id| id != new_id);
                    }
                    let _ = out.replace_container_child(parent_id, rep.old_id, new_id);
                }
            }
            let _ = out.remove_object(rep.old_id);
        }

        // Recursively substitute text/table placeholders on all remaining objects of this page
        for oid in top_ids {
            if out.has_object(oid) {
                out.substitute_object_text_and_table_placeholders(oid, |s| {
                    substitute_placeholders(s, &fields)
                });
            }
        }
    }

    Ok(out)
}

#[derive(Debug, Clone)]
enum BarcodeDirective {
    Qr(String),
    Linear {
        symbology: BarcodeSymbology,
        data: String,
    },
}

#[derive(Debug, Clone, Copy)]
enum BarcodeParent {
    Layer(LayerId),
    Container(ObjectId),
}

#[derive(Debug, Clone)]
struct BarcodeReplacement {
    old_id: ObjectId,
    parent: BarcodeParent,
    host_layer_id: LayerId,
    origin: Point2D,
    directive: BarcodeDirective,
}

#[allow(clippy::too_many_arguments)]
fn collect_barcode_replacements(
    doc: &Document,
    oid: ObjectId,
    parent: BarcodeParent,
    host_layer_id: LayerId,
    fields: &BTreeMap<String, String>,
    options: &PrintMergeOptions,
    depth: usize,
    out: &mut Vec<BarcodeReplacement>,
) {
    if depth > crate::scene_helpers::MAX_SCENE_DEPTH {
        return;
    }
    if let Some(directive) = detect_barcode_directive(doc, oid, fields, options) {
        // `doc.object_bounds(oid)` computes bounds in parent-local space (for grouped children,
        // `object_bounds` applies only `oid.transform` and not ancestor transforms, which is
        // exact because the replacement barcode group will be placed inside the same parent).
        let origin = doc
            .object_bounds(oid)
            .map_or(Point2D::ZERO, |bb| Point2D::new(bb.min_x, bb.min_y));
        out.push(BarcodeReplacement {
            old_id: oid,
            parent,
            host_layer_id,
            origin,
            directive,
        });
        return;
    }
    if let Some(obj) = doc.object(oid) {
        let children: &[ObjectId] = match &obj.payload {
            NodePayload::Group(c) => c,
            NodePayload::PowerClip { contents, .. } => contents,
            _ => return,
        };
        for &cid in children {
            collect_barcode_replacements(
                doc,
                cid,
                BarcodeParent::Container(oid),
                host_layer_id,
                fields,
                options,
                depth + 1,
                out,
            );
        }
    }
}

/// Detects whether an object on the template page is a dynamic barcode/QR placeholder:
/// - Object named `"Barcode:QR:<<URL>>"`, `"Barcode:EAN13:<<Code>>"`, `"Barcode:Code128:<<Serial>>"`,
///   or `"Barcode:<<Barcode>>"`
/// - Or an `ArtisticText` named `"Barcode"` / `"Barcode:<Symbology>"` whose text contains a placeholder.
fn detect_barcode_directive(
    doc: &Document,
    oid: ObjectId,
    fields: &BTreeMap<String, String>,
    options: &PrintMergeOptions,
) -> Option<BarcodeDirective> {
    let obj = doc.object(oid)?;
    let name_trimmed = obj.name.trim();

    // Form 1: Object name starts with "Barcode:" (case-insensitive)
    if let Some(rest) = strip_prefix_ignore_ascii_case(name_trimmed, "barcode:") {
        let rest = rest.trim();
        // Either "<Symbology>:<Template>" or "<Template>"
        if let Some((sym_part, payload_part)) = rest.split_once(':') {
            let sym_upper = sym_part.trim().to_ascii_uppercase();
            if sym_upper == "QR" || sym_upper == "QRCODE" {
                let resolved = substitute_placeholders(payload_part.trim(), fields);
                if payload_part.contains("<<") && !resolved.is_empty() {
                    return Some(BarcodeDirective::Qr(resolved));
                }
            } else if let Some(sym) = BarcodeSymbology::parse_name(sym_part) {
                let resolved = substitute_placeholders(payload_part.trim(), fields);
                if payload_part.contains("<<") && !resolved.is_empty() {
                    return Some(BarcodeDirective::Linear {
                        symbology: sym,
                        data: resolved,
                    });
                }
            }
        } else if rest.contains("<<") {
            let resolved = substitute_placeholders(rest, fields);
            if !resolved.is_empty() {
                return Some(BarcodeDirective::Linear {
                    symbology: options.default_barcode_symbology,
                    data: resolved,
                });
            }
        } else if let NodePayload::ArtisticText(text) = &obj.payload {
            let raw = text.plain_text();
            if raw.contains("<<") {
                let resolved = substitute_placeholders(&raw, fields);
                if !resolved.is_empty() {
                    let sym_upper = rest.to_ascii_uppercase();
                    if sym_upper == "QR" || sym_upper == "QRCODE" {
                        return Some(BarcodeDirective::Qr(resolved));
                    }
                    let sym = BarcodeSymbology::parse_name(rest)
                        .unwrap_or(options.default_barcode_symbology);
                    return Some(BarcodeDirective::Linear {
                        symbology: sym,
                        data: resolved,
                    });
                }
            }
        }
    }

    // Form 2: ArtisticText explicitly named "Barcode" with placeholder text like "<<Barcode>>"
    if name_trimmed.eq_ignore_ascii_case("barcode") {
        if let NodePayload::ArtisticText(text) = &obj.payload {
            let raw = text.plain_text();
            if raw.contains("<<") {
                let resolved = substitute_placeholders(&raw, fields);
                if !resolved.is_empty() {
                    return Some(BarcodeDirective::Linear {
                        symbology: options.default_barcode_symbology,
                        data: resolved,
                    });
                }
            }
        }
    }

    None
}

fn strip_prefix_ignore_ascii_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    debug_assert!(prefix.is_ascii());
    let head = s.as_bytes().get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix.as_bytes()) {
        s.get(prefix.len()..)
    } else {
        None
    }
}

/// Replaces every `<<FieldName>>` occurrence in `template` using `fields`
/// (exact key match first, falling back to trimmed case-insensitive match).
#[must_use]
pub fn substitute_placeholders(template: &str, fields: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut cursor = template;

    while let Some(open) = cursor.find("<<") {
        out.push_str(&cursor[..open]);
        let after_open = &cursor[open + 2..];
        if let Some(close) = after_open.find(">>") {
            let key = after_open[..close].trim();
            if let Some(val) = lookup_field(fields, key) {
                out.push_str(val);
            } else {
                // Unknown placeholder preserved verbatim
                out.push_str("<<");
                out.push_str(&after_open[..close]);
                out.push_str(">>");
            }
            cursor = &after_open[close + 2..];
        } else {
            out.push_str("<<");
            cursor = after_open;
            break;
        }
    }
    out.push_str(cursor);
    out
}

fn lookup_field<'a>(fields: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    if let Some(v) = fields.get(key) {
        return Some(v.as_str());
    }
    fields
        .iter()
        .find_map(|(k, v)| k.trim().eq_ignore_ascii_case(key).then_some(v.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{ArtisticText, Color, OutlineStyle, SceneObject, TextDirection, TextStyle};

    #[test]
    fn rfc4180_csv_and_tsv_parse_arabic_quotes_commas_and_newlines() {
        let csv = "\u{FEFF}Name,Company,Code\r\n\"أحمد، مدير\",\"Studio \"\"Alpha\"\"\",SKU-001\r\n\"Sara\nLine2\",\"Acme, Inc.\",SKU-002\r\n";
        let table = PrintMergeTable::from_csv(csv).unwrap();
        assert_eq!(table.headers, vec!["Name", "Company", "Code"]);
        assert_eq!(table.row_count(), 2);
        assert_eq!(table.rows[0][0], "أحمد، مدير");
        assert_eq!(table.rows[0][1], "Studio \"Alpha\"");
        assert_eq!(table.rows[1][0], "Sara\nLine2");
        assert_eq!(table.rows[1][1], "Acme, Inc.");

        let tsv = "Name\tCity\nعلي\tالرياض\nMona\tCairo\n";
        let tsv_table = PrintMergeTable::from_tsv(tsv).unwrap();
        assert_eq!(tsv_table.row_count(), 2);
        assert_eq!(tsv_table.rows[0], vec!["علي", "الرياض"]);

        // Unterminated quote fails cleanly
        assert!(PrintMergeTable::from_csv("A,B\n\"unterminated,2").is_err());
    }

    #[test]
    fn apply_print_merge_handles_bilingual_text_serials_and_barcodes() {
        let csv = "Name,Barcode\n\"أحمد علي\",INV-101\n\"John, Doe\",INV-102\n\"فاطمة\",INV-103\n";
        let table = PrintMergeTable::from_csv(csv).unwrap();

        let mut tpl = Document::new_a4("Event Badge Template");
        tpl.add_object(
            SceneObject::new(
                ObjectId(0),
                "Attendee Name",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "الاسم: <<Name>> (#<<Serial>>)",
                    TextStyle {
                        direction: TextDirection::Auto,
                        fill: Color::KEY_BLACK,
                        ..TextStyle::default()
                    },
                )),
            )
            .with_transform(Affine2D::translate(20.0, 20.0))
            .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();

        // Barcode placeholder object
        tpl.add_object(
            SceneObject::new(
                ObjectId(0),
                "Barcode:Code128:<<Barcode>>",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "<<Barcode>>",
                    TextStyle::default(),
                )),
            )
            .with_transform(Affine2D::translate(20.0, 60.0))
            .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();

        let opts = PrintMergeOptions {
            serial: Some(SerialNumberConfig {
                column_name: "Serial".to_owned(),
                start: 1,
                step: 1,
                pad_width: 4,
                prefix: String::new(),
                suffix: String::new(),
            }),
            page_name_pattern: "Badge <<Serial>> - <<Name>>".to_owned(),
            ..PrintMergeOptions::default()
        };

        let merged = apply_print_merge(&tpl, &table, &opts).unwrap();
        assert_eq!(merged.pages().len(), 3);
        assert_eq!(merged.pages()[0].name, "Badge 0001 - أحمد علي");
        assert_eq!(merged.pages()[1].name, "Badge 0002 - John, Doe");
        assert_eq!(merged.pages()[2].name, "Badge 0003 - فاطمة");

        // Verify substituted Arabic + serial text on page 0 and generated vector barcode group!
        let p0_objs: Vec<&SceneObject> = merged.pages()[0].layers[0]
            .objects
            .iter()
            .filter_map(|id| merged.object(*id))
            .collect();
        let text_obj = p0_objs
            .iter()
            .find(|o| o.name == "Attendee Name")
            .expect("text object");
        let NodePayload::ArtisticText(ref at) = text_obj.payload else {
            panic!("expected ArtisticText");
        };
        assert_eq!(at.plain_text(), "الاسم: أحمد علي (#0001)");

        let bc_grp = p0_objs
            .iter()
            .find(|o| o.name == "Barcode:Code128:INV-101")
            .expect("regenerated vector barcode group");
        assert!(matches!(bc_grp.payload, NodePayload::Group(_)));
    }

    #[test]
    fn pure_serial_range_generates_padded_numbers() {
        let cfg = SerialNumberConfig {
            column_name: "Ticket".to_owned(),
            start: 1,
            step: 1,
            pad_width: 4,
            prefix: "TKT-".to_owned(),
            suffix: String::new(),
        };
        let table = PrintMergeTable::from_serial_range(&cfg, 100);
        assert_eq!(table.row_count(), 100);
        assert_eq!(table.rows[0][0], "TKT-0001");
        assert_eq!(table.rows[99][0], "TKT-0100");
    }
}
