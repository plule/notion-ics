use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
};

use config::Config;
use icalendar::*;
use notion::{
    ids::PropertyId,
    models::{
        page::UpdatePageQuery,
        properties::{self, DateOrDateTime, DateValue, PropertyValue, WritePropertyValue},
        search::{DatabaseQuery, FilterCondition, NotionSearch, PropertyCondition, TextCondition},
        text::{RichText, RichTextCommon, Text},
        Database, Page, PageCreateRequest, Parent, Properties, WriteProperties,
    },
    *,
};
use serde::Deserialize;
use tracing::{debug, info};

#[derive(Debug, Deserialize)]
struct Settings {
    pub ical_url: String,
    pub notion_token: String,
    pub notion_calendar: String,
    pub id_property: String,
    pub date_property: String,
    pub location_property: Option<String>,
}

struct Sync<'a> {
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

        match (event.get_location(), self.location_property.clone()) {
            (Some(location), Some(property)) => {
                properties.insert(property.to_string(), text_write_property(location));
            }
            _ => {}
        }

        WriteProperties { properties }
    }

    /// Create a page based on an event
    async fn create(&mut self, event: &Event) {
        info!("Creating {}", event.get_summary().unwrap_or_default());
        let properties = page_properties(self.write_properties(event));

        let request = PageCreateRequest {
            parent: Parent::Database {
                database_id: self.database.id.clone(),
            },
            properties,
        };
        self.notion.create_page(request).await.unwrap();
    }

    /// Update a page based on an event
    async fn update(&mut self, ical_event: &Event, notion_event: &Page) {
        debug!("Updating");
        let properties = self.write_properties(ical_event);

        // Filter out properties that are already up to date
        let properties: HashMap<String, WritePropertyValue> = properties
            .properties
            .into_iter()
            .filter(|(name, value)| {
                let property = notion_event.properties.properties.get(name);
                if let Some(property) = property {
                    let equal = property_comp(property, value);
                    if !equal {
                        info!("{}: {:?} != {:?}", name, property, value);
                    }
                    !equal
                } else {
                    true
                }
            })
            .collect();

        if properties.is_empty() {
            info!(
                "{} is up to date",
                ical_event.get_summary().unwrap_or_default()
            );
            return;
        }

        info!("Updating {}", ical_event.get_summary().unwrap_or_default());
        let query = UpdatePageQuery {
            properties: Some(WriteProperties { properties }),
            ..Default::default()
        };

        self.notion
            .update_page(&notion_event.id, query)
            .await
            .unwrap();
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    info!("Reading configuration");
    let settings = Config::builder()
        .add_source(config::File::with_name("settings"))
        .add_source(config::Environment::with_prefix("NOTION_ICS"))
        .build()
        .unwrap()
        .try_deserialize::<Settings>()
        .unwrap();

    info!("Fetching calendar");
    let calendar = reqwest::get(&settings.ical_url)
        .await
        .expect("Failed to fetch calendar")
        .text()
        .await
        .expect("Failed to read calendar")
        .parse::<Calendar>()
        .expect("Failed to parse calendar");

    let ical_events: HashMap<String, &Event> = calendar
        .into_iter()
        .rev()
        .filter_map(|ev| ev.as_event())
        .map(|ev| (ev.get_uid().unwrap_or_default().to_string(), ev))
        .collect();

    info!("Fetching Notion database");
    let client = NotionApi::new(settings.notion_token.clone()).unwrap();
    let query = NotionSearch::Query(settings.notion_calendar.clone());
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
        filter: Some(FilterCondition::Property {
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

    let ical_ids = ical_events.keys().cloned().collect::<HashSet<_>>();
    let notion_ids = notion_events.keys().cloned().collect::<HashSet<_>>();
    let ids: Vec<String> = ical_ids.union(&notion_ids).cloned().collect();

    let mut sync = Sync {
        notion: &client,
        database,
        title_property: &title_property,
        id_property: &settings.id_property,
        date_property: &settings.date_property,
        location_property: settings.location_property.as_deref(),
    };

    for id in ids {
        match (ical_events.get(&id), notion_events.get(&id)) {
            (Some(ical_event), Some(notion_event)) => {
                sync.update(ical_event, notion_event).await;
            }
            (Some(ical_event), None) => {
                sync.create(ical_event).await;
            }
            (None, Some(_notion_event)) => {
                debug!("Event {} is in Notion but not in ICS", id);
            }
            (None, None) => {
                unreachable!()
            }
        }
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

fn rich_text_comp(a: &Vec<RichText>, b: &Vec<RichText>) -> bool {
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
        (
            PropertyValue::Relation { relation, .. },
            WritePropertyValue::Relation {
                relation: new_relation,
            },
        ) => relation == new_relation,
        (
            PropertyValue::People { people, .. },
            WritePropertyValue::People { people: new_people },
        ) => people == new_people,
        (PropertyValue::Files { files, .. }, WritePropertyValue::Files { files: new_files }) => {
            files == new_files
        }
        (
            PropertyValue::Checkbox { checkbox, .. },
            WritePropertyValue::Checkbox {
                checkbox: new_checkbox,
            },
        ) => checkbox == new_checkbox,
        (PropertyValue::Url { url, .. }, WritePropertyValue::Url { url: new_url }) => {
            url == new_url
        }
        (PropertyValue::Email { email, .. }, WritePropertyValue::Email { email: new_email }) => {
            email == new_email
        }
        (
            PropertyValue::PhoneNumber { phone_number, .. },
            WritePropertyValue::PhoneNumber {
                phone_number: new_phone_number,
            },
        ) => phone_number == new_phone_number,
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
