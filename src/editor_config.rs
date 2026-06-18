use shared::absolute_path::AbsolutePath;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IndentSettings {
    char: char,
    width: usize,
}

impl IndentSettings {
    pub(crate) fn new(char: char, width: usize) -> Self {
        Self { char, width }
    }

    pub(crate) fn char(self) -> char {
        self.char
    }

    pub(crate) fn width(self) -> usize {
        self.width
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EditorConfigSettings {
    indent_char: Option<char>,
    indent_width: Option<usize>,
}

impl EditorConfigSettings {
    pub(crate) fn from_path(path: &AbsolutePath) -> Self {
        match editorconfig_core::properties(path.as_ref()) {
            Ok(properties) => Self::from_properties(&properties),
            Err(error) => {
                log::warn!("Failed to load EditorConfig for {path:?}: {error:?}");
                Self::default()
            }
        }
    }

    pub(crate) fn indent_settings(&self, fallback: IndentSettings) -> IndentSettings {
        IndentSettings::new(
            self.indent_char.unwrap_or(fallback.char()),
            self.indent_width.unwrap_or(fallback.width()),
        )
    }

    fn from_properties(properties: &std::collections::HashMap<String, String>) -> Self {
        let indent_style = property(properties, "indent_style");
        let indent_size = property(properties, "indent_size");

        let indent_char = match indent_style {
            Some("tab") => Some('\t'),
            Some("space") => Some(' '),
            _ => None,
        };

        let indent_width = if indent_style == Some("tab") {
            Some(1)
        } else {
            indent_size.and_then(parse_width)
        };

        Self {
            indent_char,
            indent_width,
        }
    }
}

fn property<'a>(
    properties: &'a std::collections::HashMap<String, String>,
    key: &str,
) -> Option<&'a str> {
    properties
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.eq_ignore_ascii_case("unset"))
}

fn parse_width(value: &str) -> Option<usize> {
    value.parse::<usize>().ok().filter(|width| *width > 0)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{EditorConfigSettings, IndentSettings};

    fn settings(properties: &[(&str, &str)]) -> IndentSettings {
        EditorConfigSettings::from_properties(
            &properties
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect::<HashMap<_, _>>(),
        )
        .indent_settings(IndentSettings::new('\t', 8))
    }

    #[test]
    fn parses_space_indentation() {
        assert_eq!(
            settings(&[("indent_style", "space"), ("indent_size", "2")]),
            IndentSettings::new(' ', 2)
        );
    }

    #[test]
    fn parses_tab_indentation_as_one_tab_per_level() {
        assert_eq!(
            settings(&[("indent_style", "tab"), ("indent_size", "4")]),
            IndentSettings::new('\t', 1)
        );
    }

    #[test]
    fn falls_back_per_property() {
        assert_eq!(
            settings(&[("indent_style", "space")]),
            IndentSettings::new(' ', 8)
        );
        assert_eq!(
            settings(&[("indent_size", "2")]),
            IndentSettings::new('\t', 2)
        );
    }

    #[test]
    fn ignores_unset_indentation_properties() {
        assert_eq!(
            settings(&[("indent_style", "unset"), ("indent_size", "unset")]),
            IndentSettings::new('\t', 8)
        );
    }
}
