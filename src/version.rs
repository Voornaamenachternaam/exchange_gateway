// src/version.rs
//
// Single source of truth for the Exchange server version the gateway advertises
// across every protocol surface (EWS SOAP, Autodiscover V1 XML, Autodiscover
// SOAP, Autodiscover V2). Previously the version was hard-coded to
// "15.20.0.0" / "Exchange2016" independently in six+ call sites; consolidating
// it here guarantees the version stamps always agree and lets an operator pin a
// specific build via environment variables.
//
// Defaults match the latest stable on-premises release the gateway emulates:
// **Exchange Server SE** build *15.2.2562.45*, advertising the `Exchange2019`
// EWS schema (the highest valid `RequestServerVersion` enum value — there is no
// `Exchange2019_SP1` and no `ExchangeServerSE` enum synonym; Exchange Server SE
// is the 15.2.x product line and reuses the `Exchange2019` schema token).
//
// The active `ServerVersion` is installed once at process start
// (`version::init`) and is immutable thereafter; `version::current()` returns
// it (lazily defaulting to the well-known Exchange Server SE build when a
// caller — e.g. a unit test — never calls `init`, so leaf render helpers in
// `ews.rs`/`autodiscover.rs`/`protocol_fixtures.rs` always emit a valid version
// stamp with no per-caller plumbing).

use std::sync::OnceLock;

use crate::util::xml_escape;

/// Product name advertised to clients that surface a server product label.
pub const PRODUCT_NAME: &str = "Exchange Server SE";

/// Major version of the default build (Exchange Server SE 15.2.2562.45).
pub const DEFAULT_MAJOR: u16 = 15;
/// Minor version of the default build.
pub const DEFAULT_MINOR: u16 = 2;
/// Major build number of the default build.
pub const DEFAULT_BUILD: u32 = 2562;
/// Minor build (revision) number of the default build.
pub const DEFAULT_MINOR_BUILD: u16 = 45;
/// EWS `RequestServerVersion` enum token advertised by the default build.
pub const DEFAULT_EXCHANGE_VERSION: &str = "Exchange2019";

/// Dot-delimited default version string used by the Autodiscover outlook
/// `<ServerVersion>` element ("Major.Minor.Build.Revision").
pub const DEFAULT_VERSION_STRING: &str = "15.2.2562.45";

/// The complete, ordered set of EWS schema versions advertised under the
/// Autodiscover SOAP `EwsSupportedSchemas` user setting. Ordered oldest →
/// newest and always terminated by `Exchange2019`, mirroring the real Exchange
/// Server SE enumeration so strict clients (New Outlook, AutoDetect) accept
/// the advertised schema matrix.
pub const SUPPORTED_SCHEMAS: &[&str] = &[
    "Exchange2007",
    "Exchange2007_SP1",
    "Exchange2010",
    "Exchange2010_SP1",
    "Exchange2010_SP2",
    "Exchange2013",
    "Exchange2013_SP1",
    "Exchange2016",
    "Exchange2019",
];

/// All valid `RequestServerVersion` enum tokens accepted for the
/// `<ServerVersionInfo Version="…">` attribute / `ExternalEwsVersion`
/// `InternalEwsVersion` user settings.
const VALID_EXCHANGE_VERSIONS: &[&str] = SUPPORTED_SCHEMAS;

/// Aggregated, validated Exchange server version advertised by the gateway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerVersion {
    major: u16,
    minor: u16,
    build: u32,
    minor_build: u16,
    exchange_version: String,
}

impl ServerVersion {
    /// Build a `ServerVersion`, validating the numeric components and the EWS
    /// schema enum token. Returns an error (so `Config::validate` fails closed)
    /// on any malformed input rather than silently emitting a broken stamp.
    pub fn try_new(
        major: u16,
        minor: u16,
        build: u32,
        minor_build: u16,
        exchange_version: &str,
    ) -> Result<Self, String> {
        if major == 0 {
            return Err("server version major component must be non-zero".to_string());
        }
        if build == 0 {
            return Err("server version build number must be non-zero".to_string());
        }
        let version =
            exchange_version.trim();
        if version.is_empty() {
            return Err("server exchange version must not be empty".to_string());
        }
        if !VALID_EXCHANGE_VERSIONS
            .iter()
            .any(|&v| v.eq_ignore_ascii_case(version))
        {
            return Err(format!(
                "server exchange version '{}' is not a valid EWS RequestServerVersion token \
                 (expected one of: {})",
                version,
                VALID_EXCHANGE_VERSIONS.join(", ")
            ));
        }
        // Canonicalise the enum token to the exact-cased schema spelling
        // (e.g. tolerate "exchange2019" from env and emit "Exchange2019").
        let canonical = VALID_EXCHANGE_VERSIONS
            .iter()
            .copied()
            .find(|&v| v.eq_ignore_ascii_case(version))
            .expect("validated above")
            .to_string();
        Ok(Self {
            major,
            minor,
            build,
            minor_build,
            exchange_version: canonical,
        })
    }

    /// Default `ServerVersion` — the well-known Exchange Server SE build.
    pub fn se() -> Self {
        Self {
            major: DEFAULT_MAJOR,
            minor: DEFAULT_MINOR,
            build: DEFAULT_BUILD,
            minor_build: DEFAULT_MINOR_BUILD,
            exchange_version: DEFAULT_EXCHANGE_VERSION.to_string(),
        }
    }

    /// Parse a "Major.Minor.Build.Revision" version string (e.g.
    /// "15.2.2562.45") into numeric components, validating each segment.
    pub fn parse_version_string(s: &str) -> Result<(u16, u16, u32, u16), String> {
        let parts: Vec<&str> = s.trim().split('.').collect();
        if parts.len() != 4 {
            return Err(format!(
                "server version '{}' must have exactly four dot-separated components \
                 (Major.Minor.Build.Revision, e.g. \"{}\")",
                s,
                DEFAULT_VERSION_STRING
            ));
        }
        let major = parts[0]
            .parse::<u16>()
            .map_err(|_| format!("server version major '{}' is not a u16", parts[0]))?;
        let minor = parts[1]
            .parse::<u16>()
            .map_err(|_| format!("server version minor '{}' is not a u16", parts[1]))?;
        let build = parts[2]
            .parse::<u32>()
            .map_err(|_| format!("server version build '{}' is not a u32", parts[2]))?;
        let minor_build = parts[3]
            .parse::<u16>()
            .map_err(|_| format!("server version revision '{}' is not a u16", parts[3]))?;
        Ok((major, minor, build, minor_build))
    }

    /// Construct from config-provided strings: a "Major.Minor.Build.Revision"
    /// version string plus an EWS exchange-version token, validating both.
    pub fn from_strings(version_string: &str, exchange_version: &str) -> Result<Self, String> {
        let (major, minor, build, minor_build) = Self::parse_version_string(version_string)?;
        Self::try_new(major, minor, build, minor_build, exchange_version)
    }

    pub fn major(&self) -> u16 {
        self.major
    }
    pub fn minor(&self) -> u16 {
        self.minor
    }
    pub fn build(&self) -> u32 {
        self.build
    }
    pub fn minor_build(&self) -> u16 {
        self.minor_build
    }
    /// EWS schema enum token (e.g. "Exchange2019") used in the `Version`
    /// attribute and the `ExternalEwsVersion`/`InternalEwsVersion` settings.
    pub fn exchange_version(&self) -> &str {
        &self.exchange_version
    }
    /// Dot-delimited version string ("Major.Minor.Build.Revision"), e.g.
    /// "15.2.2562.45" — used by the Autodiscover outlook `<ServerVersion>`.
    pub fn version_string(&self) -> String {
        format!("{}.{}.{}.{}", self.major, self.minor, self.build, self.minor_build)
    }

    /// The comma-separated `EwsSupportedSchemas` user-setting value advertised
    /// in the Autodiscover SOAP response.
    pub fn supported_schemas_csv(&self) -> String {
        SUPPORTED_SCHEMAS.join(",")
    }

    /// Render the EWS SOAP header `<t:ServerVersionInfo … />` self-closing
    /// element. `type_ns` is the EWS types namespace.
    pub fn render_ews_header(&self, type_ns: &str) -> String {
        format!(
            r#"<t:ServerVersionInfo MajorVersion="{}" MinorVersion="{}" MajorBuildNumber="{}" MinorBuildNumber="{}" Version="{}" xmlns:t="{}" />"#,
            self.major,
            self.minor,
            self.build,
            self.minor_build,
            xml_escape(self.exchange_version.as_str()),
            xml_escape(type_ns),
        )
    }

    /// Render the Autodiscover SOAP header `<a:ServerVersionInfo … />` element
    /// (no `xmlns:t` — the Autodiscover 2010 namespace prefixes the element).
    pub fn render_autodiscover_soap_header(&self) -> String {
        format!(
            r#"<a:ServerVersionInfo MajorVersion="{}" MinorVersion="{}" MajorBuildNumber="{}" MinorBuildNumber="{}" Version="{}" />"#,
            self.major,
            self.minor,
            self.build,
            self.minor_build,
            xml_escape(self.exchange_version.as_str()),
        )
    }

    /// Render the Autodiscover outlook `<ServerVersion>` element value
    /// ("Major.Minor.Build.Revision").
    pub fn render_server_version_element(&self) -> String {
        self.version_string()
    }
}

// ---------------------------------------------------------------------------
// Process-wide active version.
// ---------------------------------------------------------------------------

static CURRENT: OnceLock<ServerVersion> = OnceLock::new();

/// `Default` produces the well-known Exchange Server SE build, so `version`
/// values constructed via `Default::default()` agree with the SE stamps the
/// gateway advertises by default.
impl Default for ServerVersion {
    fn default() -> Self {
        Self::se()
    }
}

/// Install the active process-wide `ServerVersion`. Called exactly once during
/// gateway startup from `Config` (validated). Subsequent calls are ignored and
/// log a warning rather than panicking, so a re-entrant `init` in tests does not
/// abort the process; the first installed value wins, which is the correct
/// fail-closed posture (a misconfigured late override cannot mutate the version
/// stamps already advertised).
pub fn init(version: ServerVersion) {
    if CURRENT.set(version).is_err() {
        tracing::warn!(
            "Server version already initialised; ignoring re-init (first value wins)"
        );
    }
}

/// Borrow the active `ServerVersion`. Lazily installs the default Exchange
/// Server SE build when no caller has run `init` (e.g. unit tests), so every
/// render helper always emits a valid stamp.
pub fn current() -> &'static ServerVersion {
    CURRENT.get_or_init(ServerVersion::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_exchange_server_se_15_2_2562_45() {
        let v = ServerVersion::default();
        assert_eq!(v.major(), 15);
        assert_eq!(v.minor(), 2);
        assert_eq!(v.build(), 2562);
        assert_eq!(v.minor_build(), 45);
        assert_eq!(v.exchange_version(), "Exchange2019");
        assert_eq!(v.version_string(), "15.2.2562.45");
    }

    #[test]
    fn default_version_string_constant_matches_default() {
        assert_eq!(ServerVersion::default().version_string(), DEFAULT_VERSION_STRING);
    }

    #[test]
    fn try_new_rejects_zero_major_or_build() {
        assert!(ServerVersion::try_new(0, 2, 2562, 45, "Exchange2019").is_err());
        assert!(ServerVersion::try_new(15, 2, 0, 45, "Exchange2019").is_err());
    }

    #[test]
    fn try_new_rejects_unknown_exchange_version_token() {
        assert!(ServerVersion::try_new(15, 2, 2562, 45, "ExchangeServerSE").is_err());
        assert!(ServerVersion::try_new(15, 2, 2562, 45, "Exchange2019_SP1").is_err());
        assert!(ServerVersion::try_new(15, 2, 2562, 45, "").is_err());
    }

    #[test]
    fn try_new_canonicalises_case_insensitive_token() {
        let v = ServerVersion::try_new(15, 2, 2562, 45, "exchange2019").unwrap();
        assert_eq!(v.exchange_version(), "Exchange2019");
    }

    #[test]
    fn parse_version_string_roundtrip() {
        let (maj, min, bld, mb) = ServerVersion::parse_version_string("15.2.2562.45").unwrap();
        assert_eq!((maj, min, bld, mb), (15, 2, 2562, 45));
    }

    #[test]
    fn parse_version_string_rejects_wrong_arity_and_non_numeric() {
        assert!(ServerVersion::parse_version_string("15.2.2562").is_err());
        assert!(ServerVersion::parse_version_string("15.2.2562.45.1").is_err());
        assert!(ServerVersion::parse_version_string("15.x.2562.45").is_err());
    }

    #[test]
    fn from_strings_builds_valid_version() {
        let v = ServerVersion::from_strings("15.2.2562.45", "Exchange2019").unwrap();
        assert_eq!(v.exchange_version(), "Exchange2019");
        assert_eq!(v.version_string(), "15.2.2562.45");
    }

    #[test]
    fn from_strings_propagates_errors() {
        assert!(ServerVersion::from_strings("15.2.2562", "Exchange2019").is_err());
        assert!(ServerVersion::from_strings("15.2.2562.45", "Bogus").is_err());
    }

    #[test]
    fn render_ews_header_has_all_components() {
        let v = ServerVersion::default();
        let ns = "http://schemas.microsoft.com/exchange/services/2006/types";
        let header = v.render_ews_header(ns);
        assert!(header.contains(r#"MajorVersion="15""#), "{}", header);
        assert!(header.contains(r#"MinorVersion="2""#), "{}", header);
        assert!(header.contains(r#"MajorBuildNumber="2562""#), "{}", header);
        assert!(header.contains(r#"MinorBuildNumber="45""#), "{}", header);
        assert!(header.contains(r#"Version="Exchange2019""#), "{}", header);
        assert!(header.contains(&format!(r#"xmlns:t="{}""#, ns)), "{}", header);
        // No line-continuation artifact leaks into emitted XML.
        assert!(!header.contains('\\'), "{}", header);
        assert!(header.ends_with("/>"), "{}", header);
    }

    #[test]
    fn render_autodiscover_soap_header_has_no_xmlns() {
        let v = ServerVersion::default();
        let header = v.render_autodiscover_soap_header();
        assert!(header.contains(r#"Version="Exchange2019""#));
        assert!(header.contains(r#"MajorBuildNumber="2562""#));
        assert!(!header.contains("xmlns:t"), "{}", header);
        assert!(!header.contains('\\'), "{}", header);
    }

    #[test]
    fn render_server_version_element_is_dot_string() {
        assert_eq!(ServerVersion::default().render_server_version_element(), "15.2.2562.45");
    }

    #[test]
    fn supported_schemas_csv_is_ordered_and_terminates_with_2019() {
        let csv = ServerVersion::default().supported_schemas_csv();
        let entries: Vec<&str> = csv.split(',').collect();
        assert_eq!(entries, SUPPORTED_SCHEMAS);
        assert_eq!(entries.last().copied(), Some("Exchange2019"));
    }

    #[test]
    fn current_defaults_to_se_when_uninitialised() {
        // `current()` lazily seeds the default; this assertion holds regardless
        // of test execution order because the default is deterministic.
        let v = current();
        assert_eq!(v.version_string(), "15.2.2562.45");
        assert_eq!(v.exchange_version(), "Exchange2019");
    }
}
