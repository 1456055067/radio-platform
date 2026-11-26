//! OpenAPI document.

use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::api;

struct Bearer;

impl Modify for Bearer {
    fn modify(&self, doc: &mut utoipa::openapi::OpenApi) {
        let components = doc.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(title = "Radio Platform API", version = "1", license(name = "Apache-2.0")),
    modifiers(&Bearer),
    paths(
        api::identity::me, api::identity::list_agents,
        api::discovery::discover,
        api::audio::whep, api::audio::hangup, api::audio::status, api::audio::media_status, api::audio::recording,
        api::sites::list_sites, api::sites::create_site, api::sites::register_agent,
        api::scanners::list, api::scanners::create, api::scanners::get, api::scanners::update, api::scanners::remove,
        api::control::status, api::control::capabilities, api::control::telemetry,
        api::control::get_volume, api::control::set_volume, api::control::get_squelch, api::control::set_squelch,
        api::control::hold, api::control::resume, api::control::command,
        api::control::acquire_lease, api::control::release_lease,
        api::groups::list, api::groups::get, api::groups::create, api::groups::remove,
        api::groups::add_member, api::groups::remove_member,
        api::history::list_transmissions, api::history::get_transmission, api::history::list_events, api::history::list_audit,
        api::stream::live,
        api::records::transcribe, api::records::hold, api::records::delete_recording,
        api::records::transcription_status, api::records::retention_status,
        api::analytics::activity, api::analytics::top,
        api::audit::verify, api::audit::export,
    ),
    components(schemas(crate::error::Problem, crate::hub::LiveEvent))
)]
struct ApiDoc;

/// The generated document.
#[must_use]
pub fn document() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}
