//! EWS Extended Properties - Support for Extended MAPI Properties
//!
//! This module implements comprehensive support for Exchange extended properties
//! (MAPI properties) in EWS operations, including property tags, property sets,
//! and property types for both getting and setting custom properties.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Extended property identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExtendedPropertyId {
    /// Property identified by tag (property type + ID)
    PropertyTag { tag: u16 },
    /// Property identified by distinguished property set ID
    DistinguishedPropertySet {
        set_id: DistinguishedPropertySet,
        id: u32,
        property_type: PropertyType,
    },
    /// Property identified by property set GUID
    PropertySetGuid {
        guid: String,
        id: u32,
        property_type: PropertyType,
    },
    /// Property identified by property set GUID with string name
    PropertySetGuidName {
        guid: String,
        name: String,
        property_type: PropertyType,
    },
}

/// Distinguished property sets
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DistinguishedPropertySet {
    /// Meeting properties
    Meeting = 0,
    /// Appointment properties
    Appointment = 1,
    /// Common properties
    Common = 2,
    /// Public strings
    PublicStrings = 3,
    /// Address properties
    Address = 4,
    /// Internet headers
    InternetHeaders = 5,
    /// Calendar assistant
    CalendarAssistant = 6,
    /// Unified messaging
    UnifiedMessaging = 7,
    /// Task
    Task = 8,
    /// Sharing
    Sharing = 9,
    /// Log
    Log = 10,
    /// Messaging
    Messaging = 11,
    /// PostRss
    PostRss = 12,
    /// Document
    Document = 13,
    /// Note
    Note = 14,
    /// Journal
    Journal = 15,
    /// Contact
    Contact = 16,
    /// Calendar properties
    Calendar = 17,
    /// Report
    Report = 18,
    /// Remote
    Remote = 19,
    /// Attachment
    Attachment = 20,
}

impl DistinguishedPropertySet {
    pub fn as_guid(&self) -> &'static str {
        match self {
            DistinguishedPropertySet::Meeting => "00062002-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Appointment => "00062002-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Common => "00062008-0000-0000-C000-000000000046",
            DistinguishedPropertySet::PublicStrings => "00020329-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Address => "00062004-0000-0000-C000-000000000046",
            DistinguishedPropertySet::InternetHeaders => "00020386-0000-0000-C000-000000000046",
            DistinguishedPropertySet::CalendarAssistant => "00062012-0000-0000-C000-000000000046",
            DistinguishedPropertySet::UnifiedMessaging => "00062013-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Task => "00062003-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Sharing => "00062040-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Log => "0006200A-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Messaging => "00062014-0000-0000-C000-000000000046",
            DistinguishedPropertySet::PostRss => "00062041-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Document => "00062009-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Note => "0006200E-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Journal => "0006200F-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Contact => "00062004-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Calendar => "00062002-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Report => "00062010-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Remote => "00062011-0000-0000-C000-000000000046",
            DistinguishedPropertySet::Attachment => "00062042-0000-0000-C000-000000000046",
        }
    }

    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(DistinguishedPropertySet::Meeting),
            1 => Some(DistinguishedPropertySet::Appointment),
            2 => Some(DistinguishedPropertySet::Common),
            3 => Some(DistinguishedPropertySet::PublicStrings),
            4 => Some(DistinguishedPropertySet::Address),
            5 => Some(DistinguishedPropertySet::InternetHeaders),
            6 => Some(DistinguishedPropertySet::CalendarAssistant),
            7 => Some(DistinguishedPropertySet::UnifiedMessaging),
            8 => Some(DistinguishedPropertySet::Task),
            9 => Some(DistinguishedPropertySet::Sharing),
            10 => Some(DistinguishedPropertySet::Log),
            11 => Some(DistinguishedPropertySet::Messaging),
            12 => Some(DistinguishedPropertySet::PostRss),
            13 => Some(DistinguishedPropertySet::Document),
            14 => Some(DistinguishedPropertySet::Note),
            15 => Some(DistinguishedPropertySet::Journal),
            16 => Some(DistinguishedPropertySet::Contact),
            17 => Some(DistinguishedPropertySet::Calendar),
            18 => Some(DistinguishedPropertySet::Report),
            19 => Some(DistinguishedPropertySet::Remote),
            20 => Some(DistinguishedPropertySet::Attachment),
            _ => None,
        }
    }
}

/// MAPI property types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PropertyType {
    /// Application time array
    ApplicationTimeArray = 0x1007,
    /// Binary
    Binary = 0x0102,
    /// Binary array
    BinaryArray = 0x1102,
    /// Boolean
    Boolean = 0x000B,
    /// CLSID
    Clsid = 0x0048,
    /// CLSID array
    ClsidArray = 0x1048,
    /// Currency
    Currency = 0x0006,
    /// Currency array
    CurrencyArray = 0x1006,
    /// Double
    Double = 0x0005,
    /// Double array
    DoubleArray = 0x1005,
    /// Error
    Error = 0x000A,
    /// Float
    Float = 0x0004,
    /// Float array
    FloatArray = 0x1004,
    /// Integer
    Integer = 0x0003,
    /// Integer array
    IntegerArray = 0x1003,
    /// Long
    Long = 0x0014,
    /// Long array
    LongArray = 0x1014,
    /// Null
    Null = 0x0001,
    /// Object
    Object = 0x000D,
    /// Short
    Short = 0x0002,
    /// Short array
    ShortArray = 0x1002,
    /// String
    String = 0x001F,
    /// String array
    StringArray = 0x101F,
    /// System time
    SystemTime = 0x0040,
    /// System time array
    SystemTimeArray = 0x1040,
}

impl PropertyType {
    pub fn from_u16(value: u16) -> Option<Self> {
        match value {
            0x1007 => Some(PropertyType::ApplicationTimeArray),
            0x0102 => Some(PropertyType::Binary),
            0x1102 => Some(PropertyType::BinaryArray),
            0x000B => Some(PropertyType::Boolean),
            0x0048 => Some(PropertyType::Clsid),
            0x1048 => Some(PropertyType::ClsidArray),
            0x0006 => Some(PropertyType::Currency),
            0x1006 => Some(PropertyType::CurrencyArray),
            0x0005 => Some(PropertyType::Double),
            0x1005 => Some(PropertyType::DoubleArray),
            0x000A => Some(PropertyType::Error),
            0x0004 => Some(PropertyType::Float),
            0x1004 => Some(PropertyType::FloatArray),
            0x0003 => Some(PropertyType::Integer),
            0x1003 => Some(PropertyType::IntegerArray),
            0x0014 => Some(PropertyType::Long),
            0x1014 => Some(PropertyType::LongArray),
            0x0001 => Some(PropertyType::Null),
            0x000D => Some(PropertyType::Object),
            0x0002 => Some(PropertyType::Short),
            0x1002 => Some(PropertyType::ShortArray),
            0x001F => Some(PropertyType::String),
            0x101F => Some(PropertyType::StringArray),
            0x0040 => Some(PropertyType::SystemTime),
            0x1040 => Some(PropertyType::SystemTimeArray),
            _ => None,
        }
    }

    pub fn as_u16(&self) -> u16 {
        *self as u16
    }

    pub fn is_array(&self) -> bool {
        matches!(self,
            PropertyType::ApplicationTimeArray |
            PropertyType::BinaryArray |
            PropertyType::ClsidArray |
            PropertyType::CurrencyArray |
            PropertyType::DoubleArray |
            PropertyType::FloatArray |
            PropertyType::IntegerArray |
            PropertyType::LongArray |
            PropertyType::ShortArray |
            PropertyType::StringArray |
            PropertyType::SystemTimeArray
        )
    }
}

/// Extended property value
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExtendedPropertyValue {
    /// Binary value
    Binary(Vec<u8>),
    /// Binary array
    BinaryArray(Vec<Vec<u8>>),
    /// Boolean value
    Boolean(bool),
    /// CLSID value
    Clsid(String),
    /// Currency value (64-bit integer)
    Currency(i64),
    /// Double value
    Double(f64),
    /// Float value
    Float(f32),
    /// Integer value
    Integer(i32),
    /// Long value
    Long(i64),
    /// Short value
    Short(i16),
    /// String value
    String(String),
    /// String array
    StringArray(Vec<String>),
    /// System time value (ISO 8601 format)
    SystemTime(String),
    /// System time array
    SystemTimeArray(Vec<String>),
    /// Error code
    Error(u32),
}

/// Extended property structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtendedProperty {
    pub property_id: ExtendedPropertyId,
    pub value: ExtendedPropertyValue,
}

impl ExtendedProperty {
    /// Create a new extended property
    pub fn new(property_id: ExtendedPropertyId, value: ExtendedPropertyValue) -> Self {
        Self { property_id, value }
    }

    /// Create a property from a distinguished property set
    pub fn from_distinguished_set(
        set_id: DistinguishedPropertySet,
        id: u32,
        property_type: PropertyType,
        value: ExtendedPropertyValue,
    ) -> Self {
        Self {
            property_id: ExtendedPropertyId::DistinguishedPropertySet {
                set_id,
                id,
                property_type,
            },
            value,
        }
    }

    /// Create a property from a property tag
    pub fn from_property_tag(tag: u16, value: ExtendedPropertyValue) -> Self {
        Self {
            property_id: ExtendedPropertyId::PropertyTag { tag },
            value,
        }
    }

    /// Generate EWS XML for extended field URI
    pub fn to_field_uri_xml(&self) -> String {
        match &self.property_id {
            ExtendedPropertyId::PropertyTag { tag } => {
                format!("<t:ExtendedFieldURI PropertyTag=\"{}\" PropertyType=\"{}\"/>",
                    tag, self.value_type_string())
            }
            ExtendedPropertyId::DistinguishedPropertySet { set_id, id, property_type } => {
                format!("<t:ExtendedFieldURI DistinguishedPropertySetId=\"{}\" PropertyId=\"{}\" PropertyType=\"{}\"/>",
                    set_id.as_guid(), id, property_type.as_u16())
            }
            ExtendedPropertyId::PropertySetGuid { guid, id, property_type } => {
                format!("<t:ExtendedFieldURI PropertySetId=\"{}\" PropertyId=\"{}\" PropertyType=\"{}\"/>",
                    guid, id, property_type.as_u16())
            }
            ExtendedPropertyId::PropertySetGuidName { guid, name, property_type } => {
                format!("<t:ExtendedFieldURI PropertySetId=\"{}\" PropertyName=\"{}\" PropertyType=\"{}\"/>",
                    guid, xml_escape(name), property_type.as_u16())
            }
        }
    }

    /// Generate EWS XML for the value
    pub fn to_value_xml(&self) -> String {
        match &self.value {
            ExtendedPropertyValue::Binary(data) => {
                format!("<t:Value>{}</t:Value>", base64_encode(data))
            }
            ExtendedPropertyValue::BinaryArray(arr) => {
                let values: Vec<String> = arr.iter()
                    .map(|v| format!("<t:Value>{}</t:Value>", base64_encode(v)))
                    .collect();
                format!("<t:Values>{}</t:Values>", values.join(""))
            }
            ExtendedPropertyValue::Boolean(b) => {
                format!("<t:Value>{}</t:Value>", if *b { "true" } else { "false" })
            }
            ExtendedPropertyValue::Clsid(s) => {
                format!("<t:Value>{}</t:Value>", xml_escape(s))
            }
            ExtendedPropertyValue::Currency(c) => {
                format!("<t:Value>{}</t:Value>", c)
            }
            ExtendedPropertyValue::Double(d) => {
                format!("<t:Value>{}</t:Value>", d)
            }
            ExtendedPropertyValue::Float(f) => {
                format!("<t:Value>{}</t:Value>", f)
            }
            ExtendedPropertyValue::Integer(i) => {
                format!("<t:Value>{}</t:Value>", i)
            }
            ExtendedPropertyValue::Long(l) => {
                format!("<t:Value>{}</t:Value>", l)
            }
            ExtendedPropertyValue::Short(s) => {
                format!("<t:Value>{}</t:Value>", s)
            }
            ExtendedPropertyValue::String(s) => {
                format!("<t:Value>{}</t:Value>", xml_escape(s))
            }
            ExtendedPropertyValue::StringArray(arr) => {
                let values: Vec<String> = arr.iter()
                    .map(|v| format!("<t:Value>{}</t:Value>", xml_escape(v)))
                    .collect();
                format!("<t:Values>{}</t:Values>", values.join(""))
            }
            ExtendedPropertyValue::SystemTime(t) => {
                format!("<t:Value>{}</t:Value>", xml_escape(t))
            }
            ExtendedPropertyValue::SystemTimeArray(arr) => {
                let values: Vec<String> = arr.iter()
                    .map(|v| format!("<t:Value>{}</t:Value>", xml_escape(v)))
                    .collect();
                format!("<t:Values>{}</t:Values>", values.join(""))
            }
            ExtendedPropertyValue::Error(e) => {
                format!("<t:Value>{}</t:Value>", e)
            }
        }
    }

    /// Generate complete EWS XML for extended property
    pub fn to_ews_xml(&self) -> String {
        format!(
            "<t:ExtendedProperty>{}<t:Value>{}</t:Value></t:ExtendedProperty>",
            self.to_field_uri_xml(),
            self.to_value_xml()
        )
    }

    /// Get value type string for property tag
    fn value_type_string(&self) -> String {
        match &self.value {
            ExtendedPropertyValue::Binary(_) => "Binary".to_string(),
            ExtendedPropertyValue::BinaryArray(_) => "BinaryArray".to_string(),
            ExtendedPropertyValue::Boolean(_) => "Boolean".to_string(),
            ExtendedPropertyValue::Clsid(_) => "Clsid".to_string(),
            ExtendedPropertyValue::Currency(_) => "Currency".to_string(),
            ExtendedPropertyValue::Double(_) => "Double".to_string(),
            ExtendedPropertyValue::Float(_) => "Float".to_string(),
            ExtendedPropertyValue::Integer(_) => "Integer".to_string(),
            ExtendedPropertyValue::Long(_) => "Long".to_string(),
            ExtendedPropertyValue::Short(_) => "Short".to_string(),
            ExtendedPropertyValue::String(_) => "String".to_string(),
            ExtendedPropertyValue::StringArray(_) => "StringArray".to_string(),
            ExtendedPropertyValue::SystemTime(_) => "SystemTime".to_string(),
            ExtendedPropertyValue::SystemTimeArray(_) => "SystemTimeArray".to_string(),
            ExtendedPropertyValue::Error(_) => "Error".to_string(),
        }
    }
}

/// Extended property collection
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtendedPropertyCollection {
    properties: HashMap<ExtendedPropertyId, ExtendedProperty>,
}

impl ExtendedPropertyCollection {
    /// Create a new empty collection
    pub fn new() -> Self {
        Self {
            properties: HashMap::new(),
        }
    }

    /// Add a property to the collection
    pub fn add(&mut self, property: ExtendedProperty) {
        self.properties.insert(property.property_id.clone(), property);
    }

    /// Get a property by ID
    pub fn get(&self, property_id: &ExtendedPropertyId) -> Option<&ExtendedProperty> {
        self.properties.get(property_id)
    }

    /// Remove a property
    pub fn remove(&mut self, property_id: &ExtendedPropertyId) -> Option<ExtendedProperty> {
        self.properties.remove(property_id)
    }

    /// Check if collection contains a property
    pub fn contains(&self, property_id: &ExtendedPropertyId) -> bool {
        self.properties.contains_key(property_id)
    }

    /// Get all properties
    pub fn all(&self) -> Vec<&ExtendedProperty> {
        self.properties.values().collect()
    }

    /// Get property count
    pub fn len(&self) -> usize {
        self.properties.len()
    }

    /// Check if collection is empty
    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    /// Clear all properties
    pub fn clear(&mut self) {
        self.properties.clear();
    }

    /// Generate EWS XML for all properties
    pub fn to_ews_xml(&self) -> String {
        self.properties.values()
            .map(|p| p.to_ews_xml())
            .collect()
    }

    /// Merge another collection into this one
    pub fn merge(&mut self, other: ExtendedPropertyCollection) {
        for (_, property) in other.properties {
            self.add(property);
        }
    }
}

/// Common extended property definitions
pub mod common_properties {
    use super::*;

    /// Get the importance property (0-2, where 2 is high)
    pub fn importance(value: i32) -> ExtendedProperty {
        ExtendedProperty::from_property_tag(
            0x0017,
            ExtendedPropertyValue::Integer(value),
        )
    }

    /// Get the sensitivity property (0-3)
    pub fn sensitivity(value: i32) -> ExtendedProperty {
        ExtendedProperty::from_property_tag(
            0x0036,
            ExtendedPropertyValue::Integer(value),
        )
    }

    /// Get the follow-up flag property
    pub fn flag_request(value: &str) -> ExtendedProperty {
        ExtendedProperty::from_distinguished_set(
            DistinguishedPropertySet::Common,
            0x8530,
            PropertyType::String,
            ExtendedPropertyValue::String(value.to_string()),
        )
    }

    /// Get the companies property
    pub fn companies(values: Vec<String>) -> ExtendedProperty {
        ExtendedProperty::from_distinguished_set(
            DistinguishedPropertySet::Common,
            0x8539,
            PropertyType::StringArray,
            ExtendedPropertyValue::StringArray(values),
        )
    }

    /// Get the mileage property
    pub fn mileage(value: &str) -> ExtendedProperty {
        ExtendedProperty::from_distinguished_set(
            DistinguishedPropertySet::Common,
            0x8534,
            PropertyType::String,
            ExtendedPropertyValue::String(value.to_string()),
        )
    }

    /// Get the billing information property
    pub fn billing_information(value: &str) -> ExtendedProperty {
        ExtendedProperty::from_distinguished_set(
            DistinguishedPropertySet::Common,
            0x8535,
            PropertyType::String,
            ExtendedPropertyValue::String(value.to_string()),
        )
    }
}

/// Helper functions
fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn base64_encode(data: &[u8]) -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    STANDARD.encode(data)
}

/// Extended property parser for EWS responses
pub struct ExtendedPropertyParser;

impl ExtendedPropertyParser {
    /// Parse extended property from XML element
    pub fn parse_from_xml(xml: &str) -> Result<ExtendedProperty, String> {
        // Simplified parsing - in production, use a proper XML parser
        // This is a placeholder implementation
        Err("XML parsing not implemented".to_string())
    }

    /// Parse property type from string
    pub fn parse_property_type(s: &str) -> Option<PropertyType> {
        match s {
            "Binary" => Some(PropertyType::Binary),
            "BinaryArray" => Some(PropertyType::BinaryArray),
            "Boolean" => Some(PropertyType::Boolean),
            "Clsid" => Some(PropertyType::Clsid),
            "Currency" => Some(PropertyType::Currency),
            "Double" => Some(PropertyType::Double),
            "Float" => Some(PropertyType::Float),
            "Integer" => Some(PropertyType::Integer),
            "Long" => Some(PropertyType::Long),
            "Short" => Some(PropertyType::Short),
            "String" => Some(PropertyType::String),
            "StringArray" => Some(PropertyType::StringArray),
            "SystemTime" => Some(PropertyType::SystemTime),
            "SystemTimeArray" => Some(PropertyType::SystemTimeArray),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_property_type_from_u16() {
        assert_eq!(PropertyType::from_u16(0x000B), Some(PropertyType::Boolean));
        assert_eq!(PropertyType::from_u16(0x001F), Some(PropertyType::String));
        assert_eq!(PropertyType::from_u16(0x0040), Some(PropertyType::SystemTime));
        assert_eq!(PropertyType::from_u16(0x9999), None);
    }

    #[test]
    fn test_property_type_is_array() {
        assert!(PropertyType::StringArray.is_array());
        assert!(PropertyType::BinaryArray.is_array());
        assert!(!PropertyType::String.is_array());
        assert!(!PropertyType::Integer.is_array());
    }

    #[test]
    fn test_extended_property_creation() {
        let prop = ExtendedProperty::from_property_tag(
            0x0017,
            ExtendedPropertyValue::Integer(2),
        );
        
        match &prop.property_id {
            ExtendedPropertyId::PropertyTag { tag } => {
                assert_eq!(*tag, 0x0017);
            }
            _ => panic!("Expected PropertyTag"),
        }
        
        assert_eq!(prop.value, ExtendedPropertyValue::Integer(2));
    }

    #[test]
    fn test_distinguished_property_set() {
        let prop = ExtendedProperty::from_distinguished_set(
            DistinguishedPropertySet::Common,
            0x8530,
            PropertyType::String,
            ExtendedPropertyValue::String("Follow up".to_string()),
        );
        
        match &prop.property_id {
            ExtendedPropertyId::DistinguishedPropertySet { set_id, id, property_type } => {
                assert_eq!(*set_id, DistinguishedPropertySet::Common);
                assert_eq!(*id, 0x8530);
                assert_eq!(*property_type, PropertyType::String);
            }
            _ => panic!("Expected DistinguishedPropertySet"),
        }
    }

    #[test]
    fn test_extended_property_collection() {
        let mut collection = ExtendedPropertyCollection::new();
        
        let prop1 = ExtendedProperty::from_property_tag(
            0x0017,
            ExtendedPropertyValue::Integer(2),
        );
        
        let prop2 = ExtendedProperty::from_property_tag(
            0x0036,
            ExtendedPropertyValue::Integer(1),
        );
        
        collection.add(prop1);
        collection.add(prop2);
        
        assert_eq!(collection.len(), 2);
        
        let prop_id = ExtendedPropertyId::PropertyTag { tag: 0x0017 };
        assert!(collection.contains(&prop_id));
    }

    #[test]
    fn test_common_properties() {
        let importance = common_properties::importance(2);
        assert_eq!(importance.value, ExtendedPropertyValue::Integer(2));
        
        let sensitivity = common_properties::sensitivity(1);
        assert_eq!(sensitivity.value, ExtendedPropertyValue::Integer(1));
        
        let flag = common_properties::flag_request("Follow up");
        assert_eq!(flag.value, ExtendedPropertyValue::String("Follow up".to_string()));
    }

    #[test]
    fn test_xml_escape() {
        assert_eq!(xml_escape("<test>"), "&lt;test&gt;");
        assert_eq!(xml_escape("&"), "&amp;");
        assert_eq!(xml_escape("\"quoted\""), "&quot;quoted&quot;");
    }
}
