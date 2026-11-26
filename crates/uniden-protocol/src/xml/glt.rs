//! `GLT` list replies.

use super::Element;
use crate::error::DecodeError;

/// One entry of a `GLT` list (for example an `FL`, `SYS` or `DEPT` element).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    /// Entry element name (`FL`, `SYS`, `DEPT`, `SITE`, `CFREQ`, `TGID`, ...).
    pub kind: String,
    /// `Index` attribute, used with `HLD`/`AVD`.
    pub index: Option<u32>,
    /// `Name` attribute.
    pub name: Option<String>,
    /// All attributes, verbatim.
    pub attributes: Vec<(String, String)>,
}

impl ListEntry {
    /// Returns an attribute value by name.
    #[must_use]
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A decoded `GLT` reply.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GltList {
    /// Entries in scanner order, merged across all reply parts.
    pub entries: Vec<ListEntry>,
}

impl GltList {
    /// Builds a list from a parsed `<GLT>` root.
    ///
    /// # Errors
    /// Returns an error if the root element is not `GLT`.
    pub fn from_element(root: Element) -> Result<Self, DecodeError> {
        if root.name != "GLT" {
            return Err(DecodeError::Xml(format!(
                "expected <GLT> root, found <{}>",
                root.name
            )));
        }
        let entries = root
            .children
            .into_iter()
            .map(|c| ListEntry {
                index: c.attr("Index").and_then(|v| v.trim().parse().ok()),
                name: c.attr("Name").map(str::to_owned),
                kind: c.name,
                attributes: c.attributes,
            })
            .collect();
        Ok(Self { entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::{XmlLimits, parse_documents};

    #[test]
    fn parses_spec_example() {
        // Verbatim example from BCDx36HP specification v1.03, "GLT command".
        let doc = r#"<?xml version="1.0" encoding="utf-8"?>
<GLT>
<FL Index="0" Name="Favorites List 1" Monitor="On" Q_Key="1" N_Tag="None" />
<FL Index="1" Name="Favorites List 2" Monitor="On" Q_Key="2" N_Tag="2" />
<FL Index="2" Name="Favorites List 3" Monitor="Off" Q_Key="3" N_Tag="999" />
</GLT>"#;
        let root = parse_documents(&[doc.to_owned()], &XmlLimits::default()).unwrap();
        let list = GltList::from_element(root).unwrap();
        assert_eq!(list.entries.len(), 3);
        assert_eq!(list.entries[2].index, Some(2));
        assert_eq!(list.entries[2].attr("N_Tag"), Some("999"));
        assert_eq!(list.entries[0].name.as_deref(), Some("Favorites List 1"));
    }
}
