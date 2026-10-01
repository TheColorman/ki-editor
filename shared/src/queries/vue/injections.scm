; Default script and style languages. Later patterns override explicit lang attributes.
(script_element
  (raw_text) @injection.content
  (#set! injection.language "javascript"))

((script_element
  (start_tag (attribute
    (attribute_name) @_lang
    (quoted_attribute_value (attribute_value) @_js)))
  (raw_text) @injection.content)
  (#eq? @_lang "lang")
  (#eq? @_js "js")
  (#set! injection.language "javascript"))

((script_element
  (start_tag (attribute
    (attribute_name) @_lang
    (quoted_attribute_value (attribute_value) @_ts)))
  (raw_text) @injection.content)
  (#eq? @_lang "lang")
  (#eq? @_ts "ts")
  (#set! injection.language "typescript"))

(script_element
  (start_tag (attribute
    (attribute_name) @_lang
    (quoted_attribute_value (attribute_value) @injection.language)))
  (raw_text) @injection.content
  (#eq? @_lang "lang")
  (#any-of? @injection.language "tsx" "jsx"))

(style_element
  (raw_text) @injection.content
  (#set! injection.language "css"))

((style_element
  (start_tag (attribute
    (attribute_name) @_lang
    (quoted_attribute_value (attribute_value) @injection.language)))
  (raw_text) @injection.content)
  (#eq? @_lang "lang")
  (#any-of? @injection.language "css" "scss"))

((interpolation (raw_text) @injection.content)
  (#set! injection.language "typescript"))

(directive_attribute
  (quoted_attribute_value (attribute_value) @injection.content)
  (#set! injection.language "typescript"))
