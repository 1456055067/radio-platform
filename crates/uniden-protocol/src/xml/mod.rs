//! Safe, bounded XML parsing for scanner replies.
//!
//! Scanner XML is untrusted. Parsing:
//! - rejects DTDs (no entity expansion beyond the five predefined entities
//!   and numeric character references),
//! - enforces depth, element-count, attribute-count and value-length limits,
//! - merges multi-part replies (footer sequences) into a single tree and
//!   strips `Footer`/`Foot` elements.

mod glt;
mod scanner_info;

pub use glt::{GltList, ListEntry};
pub use scanner_info::{
    AvoidState, ChannelInfo, Item, Popup, PopupButton, Property, ScannerInfo, ViewDescription,
};

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::error::DecodeError;

/// Limits applied while parsing XML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XmlLimits {
    /// Maximum element nesting depth.
    pub max_depth: usize,
    /// Maximum number of elements across all parts.
    pub max_elements: usize,
    /// Maximum attributes on one element.
    pub max_attributes: usize,
    /// Maximum length of an attribute value, in bytes.
    pub max_value_len: usize,
}

impl Default for XmlLimits {
    fn default() -> Self {
        Self {
            max_depth: 16,
            max_elements: 50_000,
            max_attributes: 64,
            max_value_len: 4096,
        }
    }
}

/// A generic parsed XML element.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Element {
    /// Element name.
    pub name: String,
    /// Attributes in document order.
    pub attributes: Vec<(String, String)>,
    /// Child elements in document order.
    pub children: Vec<Element>,
}

impl Element {
    /// Returns an attribute value by name.
    #[must_use]
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Returns the first child with the given name.
    #[must_use]
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }
}

fn is_footer(name: &str) -> bool {
    name == "Footer" || name == "Foot"
}

/// Parses one or more XML documents that form a single logical reply and
/// merges them: the first root supplies the name and attributes, children of
/// every root are concatenated, and footer elements are removed.
///
/// # Errors
/// Returns an error for malformed XML, DTDs, mismatched root names across
/// parts, or exceeded limits.
pub fn parse_documents(documents: &[String], limits: &XmlLimits) -> Result<Element, DecodeError> {
    let mut merged: Option<Element> = None;
    let mut count = 0usize;
    for doc in documents {
        let root = parse_one(doc, limits, &mut count)?;
        match merged.as_mut() {
            None => merged = Some(root),
            Some(m) => {
                if m.name != root.name {
                    return Err(DecodeError::Xml(format!(
                        "multi-part reply root changed from `{}` to `{}`",
                        m.name, root.name
                    )));
                }
                m.children.extend(root.children);
            }
        }
    }
    let mut root = merged.ok_or_else(|| DecodeError::Xml("empty XML reply".into()))?;
    root.children.retain(|c| !is_footer(&c.name));
    Ok(root)
}

fn element_from(start: &BytesStart<'_>, limits: &XmlLimits) -> Result<Element, DecodeError> {
    let name = start.name().0.to_owned();
    let mut attributes = Vec::new();
    for attr in start.attributes() {
        let attr = attr.map_err(|e| DecodeError::Xml(e.to_string()))?;
        if attributes.len() >= limits.max_attributes {
            return Err(DecodeError::LimitExceeded {
                what: "XML attributes per element",
                limit: limits.max_attributes,
            });
        }
        if attr.value.len() > limits.max_value_len {
            return Err(DecodeError::LimitExceeded {
                what: "XML attribute value",
                limit: limits.max_value_len,
            });
        }
        let key = attr.key.0.to_owned();
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|e| DecodeError::Xml(format!("attribute `{key}`: {e}")))?
            .into_owned();
        attributes.push((key, value));
    }
    Ok(Element {
        name,
        attributes,
        children: Vec::new(),
    })
}

fn parse_one(doc: &str, limits: &XmlLimits, count: &mut usize) -> Result<Element, DecodeError> {
    let mut reader = Reader::from_str(doc);
    reader.config_mut().trim_text(true);
    reader.config_mut().check_end_names = true;
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    loop {
        let event = reader
            .read_event()
            .map_err(|e| DecodeError::Xml(e.to_string()))?;
        match event {
            Event::DocType(_) => {
                return Err(DecodeError::Xml(
                    "DTD declarations are not permitted".into(),
                ));
            }
            Event::Start(ref s) | Event::Empty(ref s) => {
                if root.is_some() {
                    return Err(DecodeError::Xml("content after root element".into()));
                }
                *count += 1;
                if *count > limits.max_elements {
                    return Err(DecodeError::LimitExceeded {
                        what: "XML elements",
                        limit: limits.max_elements,
                    });
                }
                let el = element_from(s, limits)?;
                if matches!(event, Event::Start(_)) {
                    if stack.len() >= limits.max_depth {
                        return Err(DecodeError::LimitExceeded {
                            what: "XML depth",
                            limit: limits.max_depth,
                        });
                    }
                    stack.push(el);
                } else if let Some(parent) = stack.last_mut() {
                    parent.children.push(el);
                } else {
                    root = Some(el);
                }
            }
            Event::End(_) => {
                let el = stack
                    .pop()
                    .ok_or_else(|| DecodeError::Xml("unbalanced end tag".into()))?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(el);
                } else {
                    root = Some(el);
                }
            }
            Event::Eof => break,
            // Declarations, comments, text, CDATA and processing instructions
            // carry no information used by this crate.
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(DecodeError::Xml("unexpected end of document".into()));
    }
    root.ok_or_else(|| DecodeError::Xml("document has no root element".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_parts_and_strips_footer() {
        let docs = vec![
            r#"<?xml version="1.0"?><GLT><SYS Index="1"/><Footer No="1" EOT="0"/></GLT>"#.into(),
            r#"<?xml version="1.0"?><GLT><SYS Index="2"/><Foot No="2" EOT="1"/></GLT>"#.into(),
        ];
        let root = parse_documents(&docs, &XmlLimits::default()).unwrap();
        assert_eq!(root.name, "GLT");
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[1].attr("Index"), Some("2"));
    }

    #[test]
    fn rejects_dangerous_or_malformed_xml() {
        let l = XmlLimits::default();
        let bad = [
            r#"<!DOCTYPE a [<!ENTITY x "y">]><a v="&x;"/>"#,
            r#"<a v="&unknown;"/>"#,
            "<a><b></a>",
            "<a>",
            "",
            "<a/><b/>",
        ];
        for b in bad {
            assert!(parse_documents(&[b.to_owned()], &l).is_err(), "{b}");
        }
    }

    #[test]
    fn enforces_limits() {
        let deep = "<a>".repeat(20) + &"</a>".repeat(20);
        assert!(parse_documents(&[deep], &XmlLimits::default()).is_err());
        let many = format!("<a>{}</a>", "<b/>".repeat(11));
        let l = XmlLimits {
            max_elements: 10,
            ..XmlLimits::default()
        };
        assert!(parse_documents(&[many], &l).is_err());
    }

    #[test]
    fn unescapes_predefined_entities() {
        let root = parse_documents(
            &[r#"<a n="Fire &amp; Medical &#x41;"/>"#.to_owned()],
            &XmlLimits::default(),
        )
        .unwrap();
        assert_eq!(root.attr("n"), Some("Fire & Medical A"));
    }

    #[test]
    fn mismatched_part_roots_rejected() {
        let docs = vec!["<A/>".to_owned(), "<B/>".to_owned()];
        assert!(parse_documents(&docs, &XmlLimits::default()).is_err());
    }
}
