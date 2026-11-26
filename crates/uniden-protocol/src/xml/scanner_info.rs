//! `GSI`/`PSI` `ScannerInfo` replies.
//!
//! Element and attribute names follow the BCDx36HP specification v1.03
//! ("PSI, GSI Element/Attribute" sheets). `UnitID@U_Id` is community-reported.
//! Every field is optional: firmware omits elements depending on mode, and
//! unavailable values are reported as `None` rather than invented.

use super::Element;
use crate::error::DecodeError;
use crate::types::Frequency;

/// Avoid status (`Off` / `T-Avoid` / `Avoid`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AvoidState {
    /// Not avoided.
    Off,
    /// Temporarily avoided.
    Temporary,
    /// Permanently avoided.
    Permanent,
}

impl AvoidState {
    fn parse(v: &str) -> Option<Self> {
        match v.trim() {
            "Off" => Some(Self::Off),
            "T-Avoid" => Some(Self::Temporary),
            "Avoid" => Some(Self::Permanent),
            _ => None,
        }
    }
}

/// Common attributes of `MonitorList`, `System`, `Department` and `Site`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Item {
    /// `Name` (empty names are reported as `None`).
    pub name: Option<String>,
    /// `Index`, valid only while `MonitorList@DB_Counter` is unchanged.
    pub index: Option<u32>,
    /// `Avoid`.
    pub avoid: Option<AvoidState>,
    /// `Hold`.
    pub hold: Option<bool>,
    /// `Q_Key` (0–99; `None` when absent or "None").
    pub quick_key: Option<u8>,
    /// `N_Tag` (0–999; `None` when absent or "None").
    pub number_tag: Option<u16>,
    /// `SystemType` (systems only).
    pub system_type: Option<String>,
    /// `Mod` (sites only).
    pub modulation: Option<String>,
}

impl Item {
    fn from_element(e: &Element) -> Self {
        Self {
            name: non_empty(e.attr("Name")),
            index: e.attr("Index").and_then(|v| v.trim().parse().ok()),
            avoid: e.attr("Avoid").and_then(AvoidState::parse),
            hold: e.attr("Hold").and_then(on_off),
            quick_key: e.attr("Q_Key").and_then(|v| v.trim().parse().ok()),
            number_tag: e.attr("N_Tag").and_then(|v| v.trim().parse().ok()),
            system_type: non_empty(e.attr("SystemType")),
            modulation: non_empty(e.attr("Mod")),
        }
    }
}

/// The channel currently shown by the scanner.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChannelInfo {
    /// Source element (`ConvFrequency`, `TGID`, `SrchFrequency`, ...).
    pub kind: String,
    /// Common attributes.
    pub item: Item,
    /// `Freq`, if present and parseable.
    pub frequency: Option<Frequency>,
    /// `Mod`.
    pub modulation: Option<String>,
    /// Talkgroup ID with the `TGID:` prefix removed; `None` for
    /// placeholders such as `---` or `None`.
    pub tgid: Option<String>,
    /// `SvcType`.
    pub service_type: Option<String>,
    /// `SAD` (sub-audio: CTCSS/DCS/NAC as displayed).
    pub sub_audio: Option<String>,
}

const CHANNEL_ELEMENTS: [&str; 7] = [
    "ConvFrequency",
    "TGID",
    "SrchFrequency",
    "CcHitsChannel",
    "ToneOutChannel",
    "WxChannel",
    "SiteFrequency",
];

/// `Property` element: receiver and UI state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Property {
    /// `VOL`.
    pub volume: Option<u8>,
    /// `SQL`.
    pub squelch: Option<u8>,
    /// `Sig` signal bars (documented 0–4).
    pub signal_bars: Option<u8>,
    /// `Rssi`. Community sources report `-999` while idle; that sentinel is
    /// mapped to `None`.
    pub rssi: Option<f32>,
    /// `Mute` (`Mute` → `true`, `Unmute` → `false`).
    pub muted: Option<bool>,
    /// `Att` (`Off` / `On` / `G-Att`).
    pub attenuator: Option<String>,
    /// `Rec`.
    pub recording: Option<bool>,
    /// `KeyLock`.
    pub key_lock: Option<bool>,
    /// `P25Status` (`None` / `Data` / `P25` per spec).
    pub p25_status: Option<String>,
    /// `WiFi`.
    pub wifi: Option<String>,
    /// `Backlight`.
    pub backlight: Option<u8>,
}

impl Property {
    fn from_element(e: &Element) -> Self {
        let num = |k: &str| e.attr(k).and_then(|v| v.trim().parse::<u8>().ok());
        Self {
            volume: num("VOL"),
            squelch: num("SQL"),
            signal_bars: num("Sig"),
            rssi: e
                .attr("Rssi")
                .and_then(|v| v.trim().parse::<f32>().ok())
                .filter(|v| v.is_finite() && *v > -999.0),
            muted: e.attr("Mute").and_then(|v| match v.trim() {
                "Mute" => Some(true),
                "Unmute" => Some(false),
                _ => None,
            }),
            attenuator: non_empty(e.attr("Att")),
            recording: e.attr("Rec").and_then(on_off),
            key_lock: e.attr("KeyLock").and_then(on_off),
            p25_status: non_empty(e.attr("P25Status")),
            wifi: non_empty(e.attr("WiFi")),
            backlight: num("Backlight"),
        }
    }
}

/// A button in a popup dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupButton {
    /// Button label.
    pub text: String,
    /// Key code to send with `KEY,<code>,P` to press it.
    pub key_code: String,
}

/// A popup screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Popup {
    /// Popup text.
    pub text: String,
    /// Buttons, if the popup is a dialog.
    pub buttons: Vec<PopupButton>,
}

/// `ViewDescription`: display areas.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ViewDescription {
    /// `InfoArea1@Text`.
    pub info_area1: Option<String>,
    /// `InfoArea2@Text`.
    pub info_area2: Option<String>,
    /// `OverWrite@Text`.
    pub overwrite: Option<String>,
    /// `PopupScreen`.
    pub popup: Option<Popup>,
    /// `PlainText@Text` lines.
    pub plain_text: Vec<String>,
}

impl ViewDescription {
    fn from_element(e: &Element) -> Self {
        let text = |name: &str| {
            e.child(name)
                .and_then(|c| c.attr("Text"))
                .map(str::to_owned)
        };
        Self {
            info_area1: text("InfoArea1"),
            info_area2: text("InfoArea2"),
            overwrite: text("OverWrite"),
            popup: e.child("PopupScreen").map(|p| Popup {
                text: p.attr("Text").unwrap_or_default().to_owned(),
                buttons: p
                    .children
                    .iter()
                    .filter(|b| b.name == "Button")
                    .map(|b| PopupButton {
                        text: b.attr("Text").unwrap_or_default().to_owned(),
                        key_code: b.attr("KeyCode").unwrap_or_default().to_owned(),
                    })
                    .collect(),
            }),
            plain_text: e
                .children
                .iter()
                .filter(|c| c.name == "PlainText")
                .map(|c| c.attr("Text").unwrap_or_default().to_owned())
                .collect(),
        }
    }
}

/// Decoded `ScannerInfo` document.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScannerInfo {
    /// `Mode` (for example `Trunk Scan Hold`).
    pub mode: Option<String>,
    /// `V_Screen` (for example `trunk_scan`).
    pub v_screen: Option<String>,
    /// `MonitorList`.
    pub monitor_list: Option<Item>,
    /// `MonitorList@DB_Counter`; indexes are invalid if this changes.
    pub db_counter: Option<u16>,
    /// `System`.
    pub system: Option<Item>,
    /// `Department`.
    pub department: Option<Item>,
    /// `Site`.
    pub site: Option<Item>,
    /// Active channel element, if any.
    pub channel: Option<ChannelInfo>,
    /// `SiteFrequency@Freq` when a trunked channel is shown.
    pub site_frequency: Option<Frequency>,
    /// `UnitID@U_Id` (community-reported) with placeholders mapped to `None`.
    pub unit_id: Option<String>,
    /// `Property`.
    pub property: Option<Property>,
    /// `ViewDescription`.
    pub view: ViewDescription,
    /// The full element tree, for attributes not modelled above.
    pub raw: Element,
}

impl ScannerInfo {
    /// Builds from a parsed `<ScannerInfo>` root.
    ///
    /// # Errors
    /// Returns an error if the root is not `ScannerInfo`.
    pub fn from_element(root: Element) -> Result<Self, DecodeError> {
        if root.name != "ScannerInfo" {
            return Err(DecodeError::Xml(format!(
                "expected <ScannerInfo> root, found <{}>",
                root.name
            )));
        }
        let item = |n: &str| root.child(n).map(Item::from_element);
        let channel = CHANNEL_ELEMENTS
            .iter()
            .filter(|n| **n != "SiteFrequency")
            .find_map(|n| root.child(n))
            .map(|e| ChannelInfo {
                kind: e.name.clone(),
                item: Item::from_element(e),
                frequency: e.attr("Freq").and_then(|f| Frequency::from_mhz_str(f).ok()),
                modulation: non_empty(e.attr("Mod")),
                tgid: e.attr("TGID").and_then(clean_tgid),
                service_type: non_empty(e.attr("SvcType")),
                sub_audio: non_empty(e.attr("SAD")),
            });
        Ok(Self {
            mode: non_empty(root.attr("Mode")),
            v_screen: non_empty(root.attr("V_Screen")),
            monitor_list: item("MonitorList"),
            db_counter: root
                .child("MonitorList")
                .and_then(|m| m.attr("DB_Counter"))
                .and_then(|v| v.trim().parse().ok()),
            system: item("System"),
            department: item("Department"),
            site: item("Site"),
            channel,
            site_frequency: root
                .child("SiteFrequency")
                .and_then(|e| e.attr("Freq"))
                .and_then(|f| Frequency::from_mhz_str(f).ok()),
            unit_id: root
                .child("UnitID")
                .and_then(|e| e.attr("U_Id"))
                .and_then(clean_unit_id),
            property: root.child("Property").map(Property::from_element),
            view: root
                .child("ViewDescription")
                .map(ViewDescription::from_element)
                .unwrap_or_default(),
            raw: root,
        })
    }

    /// The frequency being received or displayed, if known.
    #[must_use]
    pub fn active_frequency(&self) -> Option<Frequency> {
        self.channel
            .as_ref()
            .and_then(|c| c.frequency)
            .or(self.site_frequency)
    }

    /// The active talkgroup ID, if a trunked channel is shown.
    #[must_use]
    pub fn active_tgid(&self) -> Option<&str> {
        self.channel.as_ref().and_then(|c| c.tgid.as_deref())
    }

    /// True if the scanner reports the current system, department or channel
    /// as held.
    #[must_use]
    pub fn is_holding(&self) -> bool {
        [
            self.system.as_ref(),
            self.department.as_ref(),
            self.channel.as_ref().map(|c| &c.item),
        ]
        .into_iter()
        .flatten()
        .any(|i| i.hold == Some(true))
            || self.mode.as_deref().is_some_and(|m| m.ends_with("Hold"))
    }
}

fn non_empty(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn on_off(v: &str) -> Option<bool> {
    match v.trim() {
        "On" => Some(true),
        "Off" => Some(false),
        _ => None,
    }
}

fn is_placeholder(v: &str) -> bool {
    v.is_empty() || v.chars().all(|c| c == '-') || v.eq_ignore_ascii_case("None")
}

fn clean_tgid(v: &str) -> Option<String> {
    let v = v.trim();
    let v = v
        .strip_prefix("TGID:")
        .or_else(|| v.strip_prefix("TGID"))
        .unwrap_or(v)
        .trim();
    (!is_placeholder(v)).then(|| v.to_owned())
}

fn clean_unit_id(v: &str) -> Option<String> {
    let v = v.trim();
    let v = v
        .strip_prefix("UID:")
        .or_else(|| v.strip_prefix("UID"))
        .unwrap_or(v)
        .trim();
    (!is_placeholder(v)).then(|| v.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::{XmlLimits, parse_documents};

    /// Example from BCDx36HP specification v1.03 ("PSI, GSI command
    /// Summary"), with the unescaped `&` from the PDF kept as `&amp;`.
    pub(crate) const SPEC_EXAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ScannerInfo Mode="Trunk Scan Hold" V_Screen="trunk_scan">
<MonitorList Name="Full Database" Index="4294967295" ListType="FullDb" Q_Key="None" N_Tag="None" DB_Counter="3" />
<System Name="Calcasieu" Index="283" Avoid="Off" SystemType="Conventional" Q_Key="None" N_Tag="None" Hold="On" />
<Department Name="Calcasieu Parish - Parish Fire &amp; Medical" Index="286" Avoid="Off" Q_Key="None" Hold="Off" />
<ConvFrequency Name="DeQuincy Fire Department" Index="290" Avoid="Off" Freq=" 154.4150MHz" Mod="NFM" N_Tag="None" Hold="On" SvcType="Fire Dispatch" P_Ch="Off" SAS="All" SAD="None" LVL="0" IFX="Off" />
<AGC A_AGC="Off" D_AGC="Off" />
<DualWatch PRI="Off" CC="Off" WX="Off" />
<Property VOL="0" SQL="9" Sig="0" WiFi="3" Att="Off" Rec="Off" KeyLock="Off" P25Status="None" Mute="Mute" Backlight="100" Rssi="0.377" />
<ViewDescription>
<InfoArea1 Text="F0:01234-6*789" />
<InfoArea2 Text="S3:01234-6*---" />
<PopupScreen Text="Quick Save?\n"/>
</ViewDescription>
</ScannerInfo>"#;

    fn parse(doc: &str) -> ScannerInfo {
        let root = parse_documents(&[doc.to_owned()], &XmlLimits::default()).unwrap();
        ScannerInfo::from_element(root).unwrap()
    }

    #[test]
    fn parses_spec_example() {
        let info = parse(SPEC_EXAMPLE);
        assert_eq!(info.mode.as_deref(), Some("Trunk Scan Hold"));
        assert_eq!(info.db_counter, Some(3));
        let sys = info.system.as_ref().unwrap();
        assert_eq!(sys.index, Some(283));
        assert_eq!(sys.hold, Some(true));
        assert_eq!(sys.quick_key, None);
        assert_eq!(
            info.department.as_ref().unwrap().name.as_deref(),
            Some("Calcasieu Parish - Parish Fire & Medical")
        );
        let ch = info.channel.as_ref().unwrap();
        assert_eq!(ch.kind, "ConvFrequency");
        assert_eq!(ch.frequency.unwrap().hz(), 154_415_000);
        assert_eq!(ch.modulation.as_deref(), Some("NFM"));
        assert_eq!(info.active_frequency().unwrap().hz(), 154_415_000);
        let p = info.property.as_ref().unwrap();
        assert_eq!(p.volume, Some(0));
        assert_eq!(p.squelch, Some(9));
        assert_eq!(p.muted, Some(true));
        assert_eq!(p.rssi, Some(0.377));
        assert_eq!(info.view.info_area1.as_deref(), Some("F0:01234-6*789"));
        assert!(info.is_holding());
        assert_eq!(info.unit_id, None);
    }

    #[test]
    fn trunk_placeholders_are_none() {
        let info = parse(
            r#"<ScannerInfo Mode="Trunk Scan" V_Screen="trunk_scan">
<TGID Name="" TGID="TGID: ---" />
<SiteFrequency Freq=" 769.268750MHz" />
<UnitID U_Id="UID None" />
<Property Rssi="-999" />
</ScannerInfo>"#,
        );
        assert_eq!(info.active_tgid(), None);
        assert_eq!(info.unit_id, None);
        assert_eq!(info.property.unwrap().rssi, None);
        assert_eq!(info.channel.unwrap().item.name, None);
        assert_eq!(info.site_frequency.unwrap().hz(), 769_268_750);
    }

    #[test]
    fn trunk_values_are_cleaned() {
        let info = parse(
            r#"<ScannerInfo><TGID Name="PD Disp" TGID="TGID: 1234" /><UnitID U_Id="UID:501234"/></ScannerInfo>"#,
        );
        assert_eq!(info.active_tgid(), Some("1234"));
        assert_eq!(info.unit_id.as_deref(), Some("501234"));
    }

    #[test]
    fn popup_buttons() {
        let info = parse(
            r#"<ScannerInfo><ViewDescription><PopupScreen Text="USB Cable Detected"><Button Text="Yes" KeyCode="E" /><Button Text="No" KeyCode="." /></PopupScreen></ViewDescription></ScannerInfo>"#,
        );
        let popup = info.view.popup.unwrap();
        assert_eq!(popup.buttons.len(), 2);
        assert_eq!(popup.buttons[1].key_code, ".");
    }

    #[test]
    fn wrong_root_rejected() {
        let root = parse_documents(&["<GLT/>".to_owned()], &XmlLimits::default()).unwrap();
        assert!(ScannerInfo::from_element(root).is_err());
    }
}
