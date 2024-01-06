use std::{collections::HashMap, str::FromStr};

use icalendar::*;
use notion::{
    ids::PropertyId,
    models::{
        page::UpdatePageQuery,
        properties::{DateOrDateTime, DateValue, PropertyValue, WritePropertyValue},
        text::{RichText, RichTextCommon, Text},
        Database, Page, PageCreateRequest, Parent, Properties, WriteProperties,
    },
    *,
};

pub struct Sync<'a> {
    pub notion: &'a NotionApi,
    pub database: &'a Database,
    pub title_property: &'a str,
    pub id_property: &'a str,
    pub date_property: &'a str,
    pub location_property: Option<&'a str>,
}

impl Sync<'_> {
    /// Build the list of properties to write given an ical event
    fn write_properties(&self, event: &Event) -> WriteProperties {
        let mut properties: HashMap<String, WritePropertyValue> = HashMap::new();

        let new_title = event.get_summary().unwrap_or_default();
        if !new_title.is_empty() {
            properties.insert(
                self.title_property.to_string(),
                title_write_property(new_title),
            );
        }

        properties.insert(
            self.id_property.to_string(),
            text_write_property(event.get_uid().unwrap_or_default()),
        );

        properties.insert(
            self.date_property.to_string(),
            date_write_property(
                event.get_start().unwrap().clone(),
                event.get_end().unwrap().clone(),
            ),
        );

        if let (Some(location), Some(property)) = (event.get_location(), self.location_property) {
            properties.insert(property.to_string(), text_write_property(location));
        }
        WriteProperties { properties }
    }

    /// Build a page creation request given an ical event
    pub fn create_request(&self, event: &Event) -> PageCreateRequest {
        let properties = page_properties(self.write_properties(event));
        PageCreateRequest {
            parent: Parent::Database {
                database_id: self.database.id.clone(),
            },
            properties,
        }
    }

    pub fn update_request(&self, event: &Event, notion_event: &Page) -> Option<UpdatePageQuery> {
        let properties = self.write_properties(event);

        // Filter out properties that are already up to date
        let properties: HashMap<String, WritePropertyValue> = properties
            .properties
            .into_iter()
            .filter(|(name, value)| {
                let property = notion_event.properties.properties.get(name);
                if let Some(property) = property {
                    !property_comp(property, value)
                } else {
                    true
                }
            })
            .collect();

        if properties.is_empty() {
            return None;
        }

        Some(UpdatePageQuery {
            properties: Some(WriteProperties { properties }),
            ..Default::default()
        })
    }
}

/// Convert a WritePropertyValue to a PropertyValue with empty ID (not necessary in most calls)
fn page_property(write_property: WritePropertyValue) -> PropertyValue {
    let id = PropertyId::from_str("").unwrap();
    match write_property {
        WritePropertyValue::Title { title } => PropertyValue::Title { id, title },
        WritePropertyValue::Text { rich_text } => PropertyValue::Text { id, rich_text },
        WritePropertyValue::Number { number } => PropertyValue::Number { id, number },
        WritePropertyValue::Date { date } => PropertyValue::Date { id, date },
        WritePropertyValue::Relation { relation } => PropertyValue::Relation { id, relation },
        WritePropertyValue::People { people } => PropertyValue::People { id, people },
        WritePropertyValue::Files { files } => PropertyValue::Files { id, files },
        WritePropertyValue::Checkbox { checkbox } => PropertyValue::Checkbox { id, checkbox },
        WritePropertyValue::Url { url } => PropertyValue::Url { id, url },
        WritePropertyValue::Email { email } => PropertyValue::Email { id, email },
        WritePropertyValue::PhoneNumber { phone_number } => {
            PropertyValue::PhoneNumber { id, phone_number }
        }
        _ => todo!(),
    }
}

fn page_properties(write_properties: WriteProperties) -> Properties {
    let properties = write_properties
        .properties
        .into_iter()
        .map(|(name, value)| (name, page_property(value)))
        .collect();
    Properties { properties }
}

fn rich_text_comp(a: &[RichText], b: &[RichText]) -> bool {
    a.iter()
        .map(|t| t.plain_text())
        .eq(b.iter().map(|t| t.plain_text()))
}

fn property_comp(property: &PropertyValue, write_property: &WritePropertyValue) -> bool {
    match (property, write_property) {
        (PropertyValue::Title { title, .. }, WritePropertyValue::Title { title: new_title }) => {
            rich_text_comp(title, new_title)
        }
        (
            PropertyValue::Text { rich_text, .. },
            WritePropertyValue::Text {
                rich_text: new_rich_text,
            },
        ) => rich_text_comp(rich_text, new_rich_text),
        (
            PropertyValue::Number { number, .. },
            WritePropertyValue::Number { number: new_number },
        ) => number == new_number,
        (PropertyValue::Date { date, .. }, WritePropertyValue::Date { date: new_date }) => {
            date == new_date
        }
        _ => todo!(),
    }
}

fn text_write_property(text: &str) -> WritePropertyValue {
    WritePropertyValue::Text {
        rich_text: rich_text(text),
    }
}

fn date_write_property(start: DatePerhapsTime, end: DatePerhapsTime) -> WritePropertyValue {
    WritePropertyValue::Date {
        date: Some(date_range(start, end)),
    }
}

fn title_write_property(text: &str) -> WritePropertyValue {
    WritePropertyValue::Title {
        title: rich_text(text),
    }
}

fn rich_text(text: &str) -> Vec<RichText> {
    vec![RichText::Text {
        rich_text: RichTextCommon {
            plain_text: text.to_string(),
            href: None,
            annotations: None,
        },
        text: Text {
            content: text.to_string(),
            link: None,
        },
    }]
}

fn date_range(start: DatePerhapsTime, end: DatePerhapsTime) -> DateValue {
    match (start, end) {
        (DatePerhapsTime::Date(start), DatePerhapsTime::Date(end)) => {
            let end = end.pred_opt().expect("Is this the big bang or what"); // ICS is exclusive, Notion is inclusive
            DateValue {
                start: DateOrDateTime::Date(start),
                end: if start != end {
                    Some(DateOrDateTime::Date(end))
                } else {
                    None
                },
                time_zone: None,
            }
        }
        (DatePerhapsTime::DateTime(start), DatePerhapsTime::DateTime(end)) => DateValue {
            start: DateOrDateTime::DateTime(start.try_into_utc().unwrap()),
            end: Some(DateOrDateTime::DateTime(end.try_into_utc().unwrap())),
            time_zone: None,
        },
        _ => panic!("Invalid date range"),
    }
}
