//! Convert wire positions only once the document's text is available.

use crate::{buffer::Buffer, char_index_range::CharIndexRange, selection::CharIndex};
use lsp_types::{Position, PositionEncodingKind, Range};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedRange {
    pub range: Range,
    pub encoding: PositionEncodingKind,
}

impl EncodedRange {
    pub fn to_char_range(&self, buffer: &Buffer) -> anyhow::Result<CharIndexRange> {
        anyhow::ensure!(
            (self.range.start.line, self.range.start.character)
                <= (self.range.end.line, self.range.end.character),
            "Reversed LSP diagnostic range"
        );
        Ok((self.to_char(buffer, self.range.start)?..self.to_char(buffer, self.range.end)?).into())
    }

    fn to_char(&self, buffer: &Buffer, position: Position) -> anyhow::Result<CharIndex> {
        let start = buffer.line_to_char(position.line as usize)?;
        let line = buffer.get_line_by_line_index(position.line as usize)?;
        let mut units = 0;
        let mut characters = 0;
        for character in line.chars().take_while(|c| !matches!(c, '\r' | '\n')) {
            if units == position.character as usize {
                return Ok(start + characters);
            }
            units += character_width(character, &self.encoding)?;
            characters += 1;
        }
        anyhow::ensure!(
            units == position.character as usize,
            "Invalid LSP column {position:?}"
        );
        Ok(start + characters)
    }

    pub fn from_char_range(
        buffer: &Buffer,
        range: CharIndexRange,
        encoding: PositionEncodingKind,
    ) -> anyhow::Result<Self> {
        let position = |index| -> anyhow::Result<Position> {
            let position = buffer.char_to_position(index)?;
            let units = buffer
                .get_line_by_line_index(position.line)?
                .chars()
                .take(position.column)
                .try_fold(0, |units, c| {
                    Ok::<_, anyhow::Error>(units + character_width(c, &encoding)?)
                })?;
            Ok(Position::new(position.line as u32, units as u32))
        };
        Ok(Self {
            range: Range::new(position(range.start)?, position(range.end)?),
            encoding,
        })
    }
}

fn character_width(character: char, encoding: &PositionEncodingKind) -> anyhow::Result<usize> {
    if encoding == &PositionEncodingKind::UTF8 {
        Ok(character.len_utf8())
    } else if encoding == &PositionEncodingKind::UTF16 {
        Ok(character.len_utf16())
    } else if encoding == &PositionEncodingKind::UTF32 {
        Ok(1)
    } else {
        anyhow::bail!("Unsupported LSP position encoding: {encoding:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_ranges_round_trip_unicode_and_crlf() -> anyhow::Result<()> {
        let buffer = Buffer::new(None, "a🦀é\r\nnext\n");
        for (encoding, column) in [
            (PositionEncodingKind::UTF8, 5),
            (PositionEncodingKind::UTF16, 3),
            (PositionEncodingKind::UTF32, 2),
        ] {
            let encoded = EncodedRange {
                range: Range::new(Position::new(0, column), Position::new(1, 2)),
                encoding,
            };
            let range = encoded.to_char_range(&buffer)?;
            assert_eq!(range.start, CharIndex(2));
            assert_eq!(
                EncodedRange::from_char_range(&buffer, range, encoded.encoding.clone())?,
                encoded
            );
        }
        Ok(())
    }

    #[test]
    fn rejects_columns_inside_a_surrogate_pair() {
        let range = EncodedRange {
            range: Range::new(Position::new(0, 1), Position::new(0, 1)),
            encoding: PositionEncodingKind::UTF16,
        };
        assert!(range.to_char_range(&Buffer::new(None, "🦀")).is_err());
    }
}
