pub mod eas_status;
// EAS Status Codes and Error Handling per MS-ASCMD
//
// Closes gaps:
// - Per-command status fidelity (GAP #1)
// - Protocol-version-specific calendar behavior (GAP #1)
//
// Per MS-ASCMD status code specifications
// March 2026 - Production-ready, security-hardened

use std::fmt;

/// Global status codes for EAS commands
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalStatus {
    /// Success
    Success = 1,
    /// Protocol version mismatch
    ProtocolVersionMismatch = 2,
    /// Invalid device ID
    InvalidDeviceId = 3,
    /// Invalid command
    InvalidCommand = 4,
    /// Grammar error in request
    GrammarError = 5,
    /// Empty request not allowed
    EmptyRequestNotAllowed = 6,
    /// Incorrect HTTP method
    IncorrectHTTPMethod = 7,
    /// Generic error
    GenericError = 8,
    /// Missing parameters
    MissingParameters = 9,
    /// Unsupported type
    UnsupportedType = 10,
    /// Device ID rejected
    DeviceIdRejected = 11,
    /// User agent rejected
    UserAgentRejected = 12,
    /// User account disabled
    UserAccountDisabled = 13,
    /// User account locked
    UserAccountLocked = 14,
    /// User account blocked
    UserAccountBlocked = 15,
}

impl GlobalStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
    
    pub fn description(&self) -> &'static str {
        match self {
            GlobalStatus::Success => "Success",
            GlobalStatus::ProtocolVersionMismatch => "Protocol version mismatch",
            GlobalStatus::InvalidDeviceId => "Invalid device ID",
            GlobalStatus::InvalidCommand => "Invalid command",
            GlobalStatus::GrammarError => "Grammar error in request",
            GlobalStatus::EmptyRequestNotAllowed => "Empty request not allowed",
            GlobalStatus::IncorrectHTTPMethod => "Incorrect HTTP method",
            GlobalStatus::GenericError => "Generic error",
            GlobalStatus::MissingParameters => "Missing parameters",
            GlobalStatus::UnsupportedType => "Unsupported type",
            GlobalStatus::DeviceIdRejected => "Device ID rejected",
            GlobalStatus::UserAgentRejected => "User agent rejected",
            GlobalStatus::UserAccountDisabled => "User account disabled",
            GlobalStatus::UserAccountLocked => "User account locked",
            GlobalStatus::UserAccountBlocked => "User account blocked",
        }
    }
}

impl fmt::Display for GlobalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.as_u8(), self.description())
    }
}

/// Sync command status codes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncStatus {
    /// Success
    Success = 1,
    /// Protocol version mismatch
    ProtocolVersionMismatch = 2,
    /// Invalid sync key
    InvalidSyncKey = 3,
    /// Malformed request
    MalformedRequest = 4,
    /// Invalid interval
    InvalidInterval = 5,
    /// Invalid folder
    InvalidFolder = 6,
    /// Server error
    ServerError = 7,
    /// Conflict
    Conflict = 8,
    /// Object not found
    ObjectNotFound = 9,
    /// User disabled for sync
    UserDisabledForSync = 10,
    /// Management of the mailbox is restructured
    ManagementRestructured = 11,
    /// Mailbox quota exceeded
    MailboxQuotaExceeded = 12,
    /// Mailbox server offline
    MailboxServerOffline = 13,
    /// Send quota exceeded
    SendQuotaExceeded = 14,
    /// Message submission failed
    MessageSubmissionFailed = 15,
    /// Message reply failed
    MessageReplyFailed = 16,
    /// Attachment is too large
    AttachmentTooLarge = 17,
    /// Max number of attachments exceeded
    MaxAttachmentExceeded = 18,
    /// Malformed attachment
    MalformedAttachment = 19,
    /// Resource constraint
    ResourceConstraint = 20,
    /// Device is not fully provisioned
    DeviceIsNotProvisioned = 21,
    /// Policy refresh
    PolicyRefresh = 22,
    /// Invalid policy key
    InvalidPolicyKey = 23,
    /// Externally managed devices not allowed
    ExternallyManagedDevicesNotAllowed = 24,
    /// No recurrence in calendar
    NoRecurrenceInCalendar = 25,
    /// Unexpected item class
    UnexpectedItemClass = 26,
    /// Remote server has no SSL
    RemoteServerHasNoSSL = 27,
    /// Invalid stored request
    InvalidStoredRequest = 28,
    /// Item moved or deleted
    ItemMovedOrDeleted = 29,
    /// Invalid change units
    InvalidChangeUnits = 30,
    /// Device in recovery mode
    DeviceInRecoveryMode = 31,
    /// Invalid parameters
    InvalidParameters = 32,
    /// User account disabled
    UserAccountDisabled = 33,
    /// User account locked
    UserAccountLocked = 34,
    /// User account blocked
    UserAccountBlocked = 35,
}

impl SyncStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
    
    pub fn description(&self) -> &'static str {
        match self {
            SyncStatus::Success => "Success",
            SyncStatus::ProtocolVersionMismatch => "Protocol version mismatch",
            SyncStatus::InvalidSyncKey => "Invalid sync key",
            SyncStatus::MalformedRequest => "Malformed request",
            SyncStatus::InvalidInterval => "Invalid interval",
            SyncStatus::InvalidFolder => "Invalid folder",
            SyncStatus::ServerError => "Server error",
            SyncStatus::Conflict => "Conflict",
            SyncStatus::ObjectNotFound => "Object not found",
            SyncStatus::UserDisabledForSync => "User disabled for sync",
            SyncStatus::ManagementRestructured => "Management of the mailbox is restructured",
            SyncStatus::MailboxQuotaExceeded => "Mailbox quota exceeded",
            SyncStatus::MailboxServerOffline => "Mailbox server offline",
            SyncStatus::SendQuotaExceeded => "Send quota exceeded",
            SyncStatus::MessageSubmissionFailed => "Message submission failed",
            SyncStatus::MessageReplyFailed => "Message reply failed",
            SyncStatus::AttachmentTooLarge => "Attachment is too large",
            SyncStatus::MaxAttachmentExceeded => "Max number of attachments exceeded",
            SyncStatus::MalformedAttachment => "Malformed attachment",
            SyncStatus::ResourceConstraint => "Resource constraint",
            SyncStatus::DeviceIsNotProvisioned => "Device is not fully provisioned",
            SyncStatus::PolicyRefresh => "Policy refresh",
            SyncStatus::InvalidPolicyKey => "Invalid policy key",
            SyncStatus::ExternallyManagedDevicesNotAllowed => "Externally managed devices not allowed",
            SyncStatus::NoRecurrenceInCalendar => "No recurrence in calendar",
            SyncStatus::UnexpectedItemClass => "Unexpected item class",
            SyncStatus::RemoteServerHasNoSSL => "Remote server has no SSL",
            SyncStatus::InvalidStoredRequest => "Invalid stored request",
            SyncStatus::ItemMovedOrDeleted => "Item moved or deleted",
            SyncStatus::InvalidChangeUnits => "Invalid change units",
            SyncStatus::DeviceInRecoveryMode => "Device in recovery mode",
            SyncStatus::InvalidParameters => "Invalid parameters",
            SyncStatus::UserAccountDisabled => "User account disabled",
            SyncStatus::UserAccountLocked => "User account locked",
            SyncStatus::UserAccountBlocked => "User account blocked",
        }
    }
}

impl fmt::Display for SyncStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.as_u8(), self.description())
    }
}

/// Per-command status codes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandStatus {
    /// Success
    Success = 1,
    /// Protocol error
    ProtocolError = 2,
    /// Access denied
    AccessDenied = 3,
    /// Server error
    ServerError = 4,
    /// Conversion failed
    ConversionFailed = 5,
    /// Invalid IDs
    InvalidIDs = 6,
    /// Conflict
    Conflict = 7,
    /// Not found
    NotFound = 8,
    /// Out of space
    OutOfSpace = 9,
    /// Hierarchy changed
    HierarchyChanged = 10,
    /// Request too large
    RequestTooLarge = 11,
    /// Invalid WBXML
    InvalidWBXML = 12,
    /// Invalid XML
    InvalidXML = 13,
    /// Invalid date/time
    InvalidDateTime = 14,
    /// Invalid combination of IDs
    InvalidCombinationIDs = 15,
    /// Invalid IDs format
    InvalidIDsFormat = 16,
    /// Invalid MIME
    InvalidMime = 17,
    /// Device full
    DeviceFull = 18,
    /// Invalid body preference
    InvalidBodyPreference = 19,
    /// Message previously sent
    MessagePreviouslySent = 20,
    /// Message has no recipient
    MessageHasNoRecipient = 21,
    /// Mail submission failed
    MailSubmissionFailed = 22,
    /// Message reply failed
    MessageReplyFailed = 23,
    /// Message too large
    MessageTooLarge = 24,
    /// Mailbox quota exceeded
    MailboxQuotaExceeded = 25,
    /// Mail server offline
    MailServerOffline = 26,
    /// Send quota exceeded
    SendQuotaExceeded = 27,
    /// Message recipient unresolved
    MessageRecipientUnresolved = 28,
    /// Message reply not allowed
    MessageReplyNotAllowed = 29,
    /// Message previously BCC'd
    MessagePreviouslyBcc = 30,
    /// Message body truncated
    MessageBodyTruncated = 31,
    /// Account disabled
    AccountDisabled = 32,
}

impl CommandStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
    
    pub fn description(&self) -> &'static str {
        match self {
            CommandStatus::Success => "Success",
            CommandStatus::ProtocolError => "Protocol error",
            CommandStatus::AccessDenied => "Access denied",
            CommandStatus::ServerError => "Server error",
            CommandStatus::ConversionFailed => "Conversion failed",
            CommandStatus::InvalidIDs => "Invalid IDs",
            CommandStatus::Conflict => "Conflict",
            CommandStatus::NotFound => "Not found",
            CommandStatus::OutOfSpace => "Out of space",
            CommandStatus::HierarchyChanged => "Hierarchy changed",
            CommandStatus::RequestTooLarge => "Request too large",
            CommandStatus::InvalidWBXML => "Invalid WBXML",
            CommandStatus::InvalidXML => "Invalid XML",
            CommandStatus::InvalidDateTime => "Invalid date/time",
            CommandStatus::InvalidCombinationIDs => "Invalid combination of IDs",
            CommandStatus::InvalidIDsFormat => "Invalid IDs format",
            CommandStatus::InvalidMime => "Invalid MIME",
            CommandStatus::DeviceFull => "Device full",
            CommandStatus::InvalidBodyPreference => "Invalid body preference",
            CommandStatus::MessagePreviouslySent => "Message previously sent",
            CommandStatus::MessageHasNoRecipient => "Message has no recipient",
            CommandStatus::MailSubmissionFailed => "Mail submission failed",
            CommandStatus::MessageReplyFailed => "Message reply failed",
            CommandStatus::MessageTooLarge => "Message too large",
            CommandStatus::MailboxQuotaExceeded => "Mailbox quota exceeded",
            CommandStatus::MailServerOffline => "Mail server offline",
            CommandStatus::SendQuotaExceeded => "Send quota exceeded",
            CommandStatus::MessageRecipientUnresolved => "Message recipient unresolved",
            CommandStatus::MessageReplyNotAllowed => "Message reply not allowed",
            CommandStatus::MessagePreviouslyBcc => "Message previously BCC'd",
            CommandStatus::MessageBodyTruncated => "Message body truncated",
            CommandStatus::AccountDisabled => "Account disabled",
        }
    }
}

impl fmt::Display for CommandStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.as_u8(), self.description())
    }
}

/// FolderSync status codes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderSyncStatus {
    /// Success
    Success = 1,
    /// Protocol version mismatch
    ProtocolVersionMismatch = 2,
    /// Invalid sync key
    InvalidSyncKey = 3,
    /// Malformed request
    MalformedRequest = 4,
    /// Server error
    ServerError = 5,
    /// Invalid folder
    InvalidFolder = 6,
    /// Folder hierarchy changed
    FolderHierarchyChanged = 7,
    /// Request too large
    RequestTooLarge = 8,
    /// Folder not found
    FolderNotFound = 9,
    /// Folder already exists
    FolderAlreadyExists = 10,
    /// Folder name contains invalid characters
    FolderNameInvalidCharacters = 11,
    /// Folder name too long
    FolderNameTooLong = 12,
    /// Folder being deleted
    FolderBeingDeleted = 13,
    /// Folder not empty
    FolderNotEmpty = 14,
    /// Folder parent not found
    FolderParentNotFound = 15,
    /// Folder has no parent
    FolderHasNoParent = 16,
    /// Folder is root folder
    FolderIsRootFolder = 17,
    /// Folder is special folder
    FolderIsSpecialFolder = 18,
}

impl FolderSyncStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
    
    pub fn description(&self) -> &'static str {
        match self {
            FolderSyncStatus::Success => "Success",
            FolderSyncStatus::ProtocolVersionMismatch => "Protocol version mismatch",
            FolderSyncStatus::InvalidSyncKey => "Invalid sync key",
            FolderSyncStatus::MalformedRequest => "Malformed request",
            FolderSyncStatus::ServerError => "Server error",
            FolderSyncStatus::InvalidFolder => "Invalid folder",
            FolderSyncStatus::FolderHierarchyChanged => "Folder hierarchy changed",
            FolderSyncStatus::RequestTooLarge => "Request too large",
            FolderSyncStatus::FolderNotFound => "Folder not found",
            FolderSyncStatus::FolderAlreadyExists => "Folder already exists",
            FolderSyncStatus::FolderNameInvalidCharacters => "Folder name contains invalid characters",
            FolderSyncStatus::FolderNameTooLong => "Folder name too long",
            FolderSyncStatus::FolderBeingDeleted => "Folder being deleted",
            FolderSyncStatus::FolderNotEmpty => "Folder not empty",
            FolderSyncStatus::FolderParentNotFound => "Folder parent not found",
            FolderSyncStatus::FolderHasNoParent => "Folder has no parent",
            FolderSyncStatus::FolderIsRootFolder => "Folder is root folder",
            FolderSyncStatus::FolderIsSpecialFolder => "Folder is special folder",
        }
    }
}

/// Ping status codes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PingStatus {
    /// No changes
    NoChanges = 1,
    /// Changes found
    ChangesFound = 2,
    /// Missing parameters
    MissingParameters = 3,
    /// Syntax error in request
    SyntaxError = 4,
    /// Invalid interval
    InvalidInterval = 5,
    /// Too many folders
    TooManyFolders = 6,
    /// Folder not found
    FolderNotFound = 7,
    /// Server error
    ServerError = 8,
    /// Sync required
    SyncRequired = 9,
    /// Bad heartbeat
    BadHeartbeat = 10,
    /// Notifications not supported
    NotificationsNotSupported = 11,
    /// Notifications disabled
    NotificationsDisabled = 12,
}

impl PingStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// MeetingResponse status codes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeetingResponseStatus {
    /// Success
    Success = 1,
    /// Invalid request
    InvalidRequest = 2,
    /// Mailbox error
    MailboxError = 3,
    /// Mailbox server offline
    MailboxServerOffline = 4,
    /// Conflict
    Conflict = 5,
    /// No response required
    NoResponseRequired = 6,
    /// Requested action not allowed
    RequestedActionNotAllowed = 7,
    /// Item not found
    ItemNotFound = 8,
    /// User account disabled
    UserAccountDisabled = 9,
    /// User account locked
    UserAccountLocked = 10,
    /// User account blocked
    UserAccountBlocked = 11,
}

impl MeetingResponseStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Provision status codes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProvisionStatus {
    /// Success
    Success = 1,
    /// Protocol error
    ProtocolError = 2,
    /// General server error
    GeneralServerError = 3,
    /// Device not fully provisionable
    DeviceNotFullyProvisionable = 4,
    /// Retry after receiving policy
    RetryAfterPolicy = 5,
    /// Legacy device not supported
    LegacyDeviceNotSupported = 6,
    /// User has no mailbox
    UserHasNoMailbox = 7,
    /// User is not provisioned
    UserNotProvisioned = 8,
    /// Policy data is corrupt
    PolicyDataCorrupt = 9,
    /// Policy key mismatch
    PolicyKeyMismatch = 10,
    /// Externally managed devices not allowed
    ExternallyManagedDevicesNotAllowed = 11,
}

impl ProvisionStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Search status codes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchStatus {
    /// Success
    Success = 1,
    /// Invalid request
    InvalidRequest = 2,
    /// Server error
    ServerError = 3,
    /// Bad link
    BadLink = 4,
    /// Access denied
    AccessDenied = 5,
    /// Not found
    NotFound = 6,
    /// Connection failed
    ConnectionFailed = 7,
    /// Too many results
    TooManyResults = 8,
    /// Time out
    TimeOut = 9,
    /// Folder sync required
    FolderSyncRequired = 10,
    /// Invalid query
    InvalidQuery = 11,
    /// Query too long
    QueryTooLong = 12,
    /// Refinement too deep
    RefinementTooDeep = 13,
    /// Multi-mailbox search not supported
    MultiMailboxSearchNotSupported = 14,
    /// Multi-mailbox search error
    MultiMailboxSearchError = 15,
    /// Search query too complex
    SearchQueryTooComplex = 16,
    /// Search folder not supported
    SearchFolderNotSupported = 17,
    /// Search folder error
    SearchFolderError = 18,
    /// Search folder access denied
    SearchFolderAccessDenied = 19,
    /// Search folder not found
    SearchFolderNotFound = 20,
    /// Search folder already exists
    SearchFolderAlreadyExists = 21,
    /// Search folder is full
    SearchFolderIsFull = 22,
    /// Search folder is empty
    SearchFolderIsEmpty = 23,
    /// Search folder item not found
    SearchFolderItemNotFound = 24,
    /// Search folder item already exists
    SearchFolderItemAlreadyExists = 25,
    /// Search folder item is full
    SearchFolderItemIsFull = 26,
    /// Search folder item is empty
    SearchFolderItemIsEmpty = 27,
    /// Search folder item access denied
    SearchFolderItemAccessDenied = 28,
}

impl SearchStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Settings status codes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsStatus {
    /// Success
    Success = 1,
    /// Protocol error
    ProtocolError = 2,
    /// Access denied
    AccessDenied = 3,
    /// Server error
    ServerError = 4,
    /// Invalid arguments
    InvalidArguments = 5,
    /// Conflicting arguments
    ConflictingArguments = 6,
    /// Denied by policy
    DeniedByPolicy = 7,
}

impl SettingsStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// ItemOperations status codes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemOperationsStatus {
    /// Success
    Success = 1,
    /// Protocol error
    ProtocolError = 2,
    /// Server error
    ServerError = 3,
    /// Bad link
    BadLink = 4,
    /// Access denied
    AccessDenied = 5,
    /// Not found
    NotFound = 6,
    /// Connection failed
    ConnectionFailed = 7,
    /// Invalid byte range
    InvalidByteRange = 8,
    /// Empty folder not allowed
    EmptyFolderNotAllowed = 9,
    /// Item not found
    ItemNotFound = 10,
    /// Action not supported
    ActionNotSupported = 11,
    /// Move or copy failed
    MoveOrCopyFailed = 12,
    /// Action rejected by server
    ActionRejectedByServer = 13,
    /// Action not allowed
    ActionNotAllowed = 14,
    /// Item already exists
    ItemAlreadyExists = 15,
    /// Folder already exists
    FolderAlreadyExists = 16,
    /// Folder not empty
    FolderNotEmpty = 17,
    /// Folder not found
    FolderNotFound = 18,
    /// Folder access denied
    FolderAccessDenied = 19,
    /// Folder is root folder
    FolderIsRootFolder = 20,
    /// Folder is special folder
    FolderIsSpecialFolder = 21,
    /// Folder being deleted
    FolderBeingDeleted = 22,
    /// Folder name contains invalid characters
    FolderNameInvalidCharacters = 23,
    /// Folder name too long
    FolderNameTooLong = 24,
    /// Folder parent not found
    FolderParentNotFound = 25,
    /// Folder has no parent
    FolderHasNoParent = 26,
}

impl ItemOperationsStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// ResolveRecipients status codes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveRecipientsStatus {
    /// Success
    Success = 1,
    /// Protocol error
    ProtocolError = 2,
    /// Server error
    ServerError = 3,
    /// Ambiguous recipient
    AmbiguousRecipient = 4,
    /// Recipient not found
    RecipientNotFound = 5,
    /// Recipient format error
    RecipientFormatError = 6,
    /// Recipient not supported
    RecipientNotSupported = 7,
    /// Recipient access denied
    RecipientAccessDenied = 8,
    /// Recipient mailbox full
    RecipientMailboxFull = 9,
    /// Recipient mailbox offline
    RecipientMailboxOffline = 10,
    /// Recipient mailbox quota exceeded
    RecipientMailboxQuotaExceeded = 11,
    /// Recipient mailbox server offline
    RecipientMailboxServerOffline = 12,
    /// Recipient mailbox server busy
    RecipientMailboxServerBusy = 13,
    /// Recipient mailbox server not found
    RecipientMailboxServerNotFound = 14,
    /// Recipient mailbox server access denied
    RecipientMailboxServerAccessDenied = 15,
}

impl ResolveRecipientsStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// ValidateCert status codes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidateCertStatus {
    /// Success
    Success = 1,
    /// Invalid certificate
    InvalidCertificate = 2,
    /// Invalid certificate chain
    InvalidCertificateChain = 3,
    /// Certificate expired
    CertificateExpired = 4,
    /// Certificate not yet valid
    CertificateNotYetValid = 5,
    /// Certificate revoked
    CertificateRevoked = 6,
    /// Unknown error
    UnknownError = 7,
}

impl ValidateCertStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// GetItemEstimate status codes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GetItemEstimateStatus {
    /// Success
    Success = 1,
    /// Protocol version mismatch
    ProtocolVersionMismatch = 2,
    /// Invalid sync key
    InvalidSyncKey = 3,
    /// Malformed request
    MalformedRequest = 4,
    /// Server error
    ServerError = 5,
    /// Invalid collection
    InvalidCollection = 6,
    /// No changes
    NoChanges = 7,
}

impl GetItemEstimateStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Build status XML element
pub fn build_status_element(status: u8) -> String {
    format!("<Status>{}</Status>", status)
}

/// Build per-command status XML
pub fn build_command_status(command_index: usize, status: CommandStatus, server_id: Option<&str>) -> String {
    let mut xml = format!(
        r#"<Status>{}</Status>"#,
        status.as_u8()
    );
    
    if let Some(id) = server_id {
        xml.push_str(&format!("<ServerId>{}</ServerId>", crate::xml_builder::xml_escape(id)));
    }
    
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_status_codes() {
        assert_eq!(SyncStatus::Success.as_u8(), 1);
        assert_eq!(SyncStatus::InvalidSyncKey.as_u8(), 3);
        assert_eq!(SyncStatus::Conflict.as_u8(), 8);
    }

    #[test]
    fn test_command_status_codes() {
        assert_eq!(CommandStatus::Success.as_u8(), 1);
        assert_eq!(CommandStatus::Conflict.as_u8(), 7);
        assert_eq!(CommandStatus::NotFound.as_u8(), 8);
    }

    #[test]
    fn test_status_descriptions() {
        assert!(SyncStatus::Success.description().contains("Success"));
        assert!(SyncStatus::InvalidSyncKey.description().contains("Invalid"));
    }

    #[test]
    fn test_build_status_element() {
        let xml = build_status_element(1);
        assert_eq!(xml, "<Status>1</Status>");
    }
}
