use std::{collections::HashMap, str::FromStr};

use config::Config;
use icalendar::*;
use notion::{
    ids::PropertyId,
    models::{
        properties::{DateOrDateTime, DateValue, PropertyValue},
        search::{DatabaseQuery, FilterCondition, NotionSearch, PropertyCondition, TextCondition},
        text::{RichText, RichTextCommon, Text},
        Page, PageCreateRequest, Parent, Properties,
    },
    *,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Settings {
    pub notion_token: String,
    pub notion_calendar: String,
    pub id_property: String,
    pub date_property: String,
    pub location_property: Option<String>,
}

#[tokio::main]
async fn main() {
    let settings = Config::builder()
        .add_source(config::File::with_name("settings"))
        .add_source(config::Environment::with_prefix("NOTION_ICS"))
        .build()
        .unwrap()
        .try_deserialize::<Settings>()
        .unwrap();

    let calendar = std::fs::read_to_string("calendar.ics")
        .expect("Failed to read file")
        .parse::<Calendar>()
        .expect("Failed to parse calendar");
    dbg!(&calendar);
    let events = calendar
        .into_iter()
        .rev()
        .filter_map(|ev| ev.as_event())
        .take(10);

    let client = NotionApi::new(settings.notion_token).unwrap();
    let query = NotionSearch::Query(settings.notion_calendar);
    let databases = client.search(query).await.unwrap();
    let database = match databases.results.first().unwrap() {
        models::Object::Database { database } => database,
        _ => panic!("Not a database"),
    };

    let title_property = database
        .properties
        .iter()
        .filter_map(|(name, prop)| {
            if let notion::models::properties::PropertyConfiguration::Title { .. } = prop {
                Some(name)
            } else {
                None
            }
        })
        .next()
        .unwrap()
        .clone();

    let query = DatabaseQuery {
        filter: Some(FilterCondition {
            property: settings.id_property.clone(),
            condition: PropertyCondition::RichText(TextCondition::IsNotEmpty),
        }),
        ..Default::default()
    };
    let notion_events = client.query_database(&database.id, query).await.unwrap();

    let notion_events: HashMap<String, Page> = notion_events
        .results
        .into_iter()
        .map(|ev| {
            let id_property = match ev.properties.properties.get(&settings.id_property).unwrap() {
                PropertyValue::Text { rich_text, .. } => {
                    rich_text.first().unwrap().plain_text().to_string()
                }
                _ => panic!("Not a rich text"),
            };
            (id_property, ev)
        })
        .collect();

    for event in events {
        let registered_event = notion_events.get(event.get_uid().unwrap_or_default());
        match registered_event {
            Some(_) => {
                dbg!("already registered");
            }
            None => {
                dbg!("registering");
                let mut properties: HashMap<String, PropertyValue> = HashMap::new();

                properties.insert(
                    title_property.clone(),
                    new_title_property(event.get_summary().unwrap_or_default()),
                );

                properties.insert(
                    settings.id_property.clone(),
                    new_text_property(event.get_uid().unwrap_or_default()),
                );

                properties.insert(
                    settings.date_property.clone(),
                    PropertyValue::Date {
                        id: PropertyId::from_str("").unwrap(),
                        date: Some(date_range(
                            event.get_start().unwrap(),
                            event.get_end().unwrap(),
                        )),
                    },
                );

                match (event.get_location(), settings.location_property.clone()) {
                    (Some(location), Some(property)) => {
                        properties.insert(property, new_text_property(location));
                    }
                    _ => {}
                }

                let request = PageCreateRequest {
                    parent: Parent::Database {
                        database_id: database.id.clone(),
                    },
                    properties: Properties { properties },
                };
                client.create_page(request).await.unwrap();
            }
        }
    }
}

fn new_text_property(text: &str) -> PropertyValue {
    PropertyValue::Text {
        id: PropertyId::from_str("").unwrap(),
        rich_text: rich_text(text),
    }
}

fn new_title_property(text: &str) -> PropertyValue {
    PropertyValue::Title {
        id: PropertyId::from_str("").unwrap(),
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
            let end = end.pred_opt().unwrap(); // ICS is exclusive, Notion is inclusive
            DateValue {
                start: DateOrDateTime::Date(start),
                end: if start != end {
                    Some(DateOrDateTime::Date(end.pred_opt().unwrap()))
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
