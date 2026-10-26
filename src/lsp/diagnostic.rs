use super::position::EncodedRange;
use crate::{buffer::Buffer, char_index_range::CharIndexRange, quickfix_list::QuickfixLocation};

use lsp_types::DiagnosticSeverity;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: CharIndexRange,
    pub message: String,
    pub severity: Option<DiagnosticSeverity>,
    pub related_information: Option<Vec<DiagnosticRelatedInformation>>,
    pub code_description: Option<lsp_types::CodeDescription>,
    pub original_value: Option<lsp_types::Diagnostic>,
}

impl Diagnostic {
    pub fn try_from(
        buffer: &Buffer,
        value: lsp_types::Diagnostic,
        encoding: &lsp_types::PositionEncodingKind,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            range: EncodedRange {
                range: value.range,
                encoding: encoding.clone(),
            }
            .to_char_range(buffer)?,
            message: value.message.clone(),
            severity: value.severity,
            code_description: value.code_description.clone(),
            related_information: if let Some(related_information) =
                value.related_information.clone()
            {
                Some(
                    related_information
                        .into_iter()
                        .map(|related| DiagnosticRelatedInformation::new(related, encoding))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            } else {
                None
            },
            original_value: Some(value),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticRelatedInformation {
    location: QuickfixLocation,
    message: String,
}

impl DiagnosticRelatedInformation {
    fn new(
        value: lsp_types::DiagnosticRelatedInformation,
        encoding: &lsp_types::PositionEncodingKind,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            location: QuickfixLocation::Document {
                path: value
                    .location
                    .uri
                    .to_file_path()
                    .map_err(|_| anyhow::anyhow!("Invalid diagnostic URI"))?
                    .try_into()?,
                range: EncodedRange {
                    range: value.location.range,
                    encoding: encoding.clone(),
                },
            },
            message: value.message,
        })
    }
}
