//! Conversion from the SDK's `ScannerInfo` to platform telemetry.

use platform_common::{DisplayText, TelemetrySnapshot};
use time::OffsetDateTime;
use uniden_sds::protocol::xml::ScannerInfo;

/// Builds a snapshot from decoded `GSI`/`PSI` data. Unreported values stay
/// `None`.
#[must_use]
pub fn snapshot_from_info(info: &ScannerInfo, observed_at: OffsetDateTime) -> TelemetrySnapshot {
    let channel = info.channel.as_ref();
    let property = info.property.as_ref();
    TelemetrySnapshot {
        observed_at,
        mode: info.mode.clone(),
        screen: info.v_screen.clone(),
        system: info.system.as_ref().and_then(|s| s.name.clone()),
        system_index: info.system.as_ref().and_then(|s| s.index),
        system_type: info.system.as_ref().and_then(|s| s.system_type.clone()),
        department: info.department.as_ref().and_then(|d| d.name.clone()),
        department_index: info.department.as_ref().and_then(|d| d.index),
        site: info.site.as_ref().and_then(|s| s.name.clone()),
        channel: channel.and_then(|c| c.item.name.clone()),
        channel_index: channel.and_then(|c| c.item.index),
        channel_kind: channel.map(|c| c.kind.clone()),
        frequency_hz: info.active_frequency().map(|f| f.hz()),
        tgid: info.active_tgid().map(str::to_owned),
        unit_id: info.unit_id.clone(),
        modulation: channel.and_then(|c| c.modulation.clone()),
        service_type: channel.and_then(|c| c.service_type.clone()),
        signal_bars: property.and_then(|p| p.signal_bars),
        rssi: property.and_then(|p| p.rssi),
        muted: property.and_then(|p| p.muted),
        volume: property.and_then(|p| p.volume),
        squelch: property.and_then(|p| p.squelch),
        holding: info.is_holding(),
        recording: property.and_then(|p| p.recording),
        display: DisplayText {
            info_area1: info.view.info_area1.clone(),
            info_area2: info.view.info_area2.clone(),
            overwrite: info.view.overwrite.clone(),
            popup: info.view.popup.as_ref().map(|p| p.text.clone()),
        },
    }
}

/// True if two snapshots differ in anything other than `observed_at`.
#[must_use]
pub fn materially_different(a: &TelemetrySnapshot, b: &TelemetrySnapshot) -> bool {
    let mut b = b.clone();
    b.observed_at = a.observed_at;
    *a != b
}
