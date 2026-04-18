// src/util.rs

use std::fmt::{self, Display};

pub fn xml_escape(s: &str) -> String {
 s.replace('&', "&amp;")
  .replace('<', "&lt;")
  .replace('>', "&gt;")
  .replace('"', "&quot;")
  .replace('\'', "&apos;")
}

pub fn xml_escape_text(s: &str) -> String {
 s.replace('&', "&amp;")
  .replace('<', "&lt;")
  .replace('>', "&gt;")
}

pub struct EscapedXml<'a>(pub &'a str);

impl Display for EscapedXml<'_> {
 fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
  for c in self.0.chars() {
   match c {
    '&' => write!(f, "&amp;"),
    '<' => write!(f, "&lt;"),
    '>' => write!(f, "&gt;"),
    '"' => write!(f, "&quot;"),
    '\'' => write!(f, "&apos;"),
    c => write!(f, "{}", c),
   }?;
  }
  Ok(())
 }
}

pub struct EscapedXmlText<'a>(pub &'a str);

impl Display for EscapedXmlText<'_> {
 fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
  for c in self.0.chars() {
   match c {
    '&' => write!(f, "&amp;"),
    '<' => write!(f, "&lt;"),
    '>' => write!(f, "&gt;"),
    c => write!(f, "{}", c),
   }?;
  }
  Ok(())
 }
}

pub fn sanitize_path_segment(s: &str) -> String {
 s.chars()
  .map(|c| {
   if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
    c
   } else {
    '_'
   }
  })
  .collect()
}

pub fn truncate_string(s: &str, max_len: usize) -> String {
 if s.len() <= max_len {
  s.to_string()
 } else {
  // Use char_indices to safely handle multi-byte UTF-8 characters
        let target_len = max_len.saturating_sub(3);
        let end = s.char_indices()
            .take_while(|(idx, _)| *idx < target_len)
            .last()
            .map(|(idx, c)| idx + c.len_utf8())
            .unwrap_or(0);
        format!("{}...", &s[..end])
 }
}

#[cfg(test)]
mod tests {
 use super::*;

 #[test]
 fn test_xml_escape() {
  assert_eq!(xml_escape("a<b>c&d\"e'f"), "a&lt;b&gt;c&amp;d&quot;e&apos;f");
 }

 #[test]
 fn test_xml_escape_text() {
  assert_eq!(xml_escape_text("a<b>c&d"), "a&lt;b&gt;c&amp;d");
 }

 #[test]
 fn test_sanitize_path_segment() {
  assert_eq!(sanitize_path_segment("hello world"), "hello_world");
  assert_eq!(sanitize_path_segment("test/file:name"), "test_file_name");
 }

 #[test]
 fn test_truncate_string() {
  assert_eq!(truncate_string("hello", 10), "hello");
  assert_eq!(truncate_string("hello world", 8), "hello...");
 }
}
