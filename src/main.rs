use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use anyhow::{bail, Context, Result};
use clap::Parser;
use icalendar::*;
use notion::{
    models::{
        properties::PropertyValue,
        search::{DatabaseQuery, FilterCondition, NotionSearch, PropertyCondition, TextCondition},
        Page,
    },
    *,
};
use serde::Deserialize;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::{debug, info};

mod sync;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long, default_value_t = false)]
    dry_run: bool,
    #[arg(long)]
    schedule: Option<String>,
    #[arg(long, default_value = "settings.toml")]
    config: PathBuf,
}

#[derive(Debug, Deserialize, Clone)]
struct Settings {
    pub ical_url: String,
    pub day_past: i64,
    pub day_future: i64,
    pub notion_token: String,
    pub notion_calendar: String,
    pub id_property: String,
    pub date_property: String,
    pub location_property: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    info!("Reading configuration");
    let settings: Settings = toml::from_str(&std::fs::read_to_string(args.config)?)?;
    sync(&settings, args.dry_run).await?;

    if let Some(schedule) = &args.schedule {
        info!("Scheduling sync");
        let sched = JobScheduler::new().await?;
        //let settings = settings.clone();
        let job = Job::new_async(schedule.as_str(), {
            move |_uuid, _lock| {
                let settings = settings.clone();
                Box::pin(async move {
                    if let Err(err) = sync(&settings, false).await {
                        tracing::error!("Failed to sync: {}", err);
                    }
                })
            }
        })?;
        sched.add(job).await?;
        sched.start().await?;
        tokio::signal::ctrl_c().await?;
    }

    Ok(())
}

async fn sync(settings: &Settings, dry_run: bool) -> Result<()> {
    info!("Fetching calendar");
    let calendar = reqwest::get(&settings.ical_url)
        .await
        .context("Failed to fetch calendar")?
        .text()
        .await
        .context("Failed to read calendar")?
        .parse::<Calendar>()
        .expect("Failed to parse calendar");

    let ical_events: HashMap<String, &Event> = calendar
        .iter()
        .rev()
        .filter_map(|ev| ev.as_event())
        .map(|ev| (ev.get_uid().unwrap_or_default().to_string(), ev))
        .collect();

    info!("Fetching Notion database");
    let client = NotionApi::new(settings.notion_token.clone())?;
    let query = NotionSearch::Query(settings.notion_calendar.clone());
    let databases = client.search(query).await?;
    let database = match databases
        .results
        .into_iter()
        .filter_map(|obj| match obj {
            models::Object::Database { database } => Some(database),
            _ => None,
        })
        .next()
    {
        Some(db) => db,
        _ => bail!(
            "{} not found, check its name and if integrated with notion-ics",
            settings.notion_calendar
        ),
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
        .context("No title property in the database?")?
        .clone();

    let query = DatabaseQuery {
        filter: Some(FilterCondition::Property {
            property: settings.id_property.clone(),
            condition: PropertyCondition::RichText(TextCondition::IsNotEmpty),
        }),
        ..Default::default()
    };
    let notion_events = client.query_database(&database.id, query).await?;

    let notion_events: HashMap<String, Page> = notion_events
        .results
        .into_iter()
        .map(|ev| {
            let id_property = match ev.properties.properties.get(&settings.id_property) {
                Some(PropertyValue::Text { rich_text, .. }) => {
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

    let sync = sync::Sync {
        notion: &client,
        database: &database,
        title_property: &title_property,
        id_property: &settings.id_property,
        date_property: &settings.date_property,
        location_property: settings.location_property.as_deref(),
    };

    let earliest =
        (chrono::offset::Local::now() - chrono::Duration::days(settings.day_past)).date_naive();
    let latest =
        (chrono::offset::Local::now() + chrono::Duration::days(settings.day_future)).date_naive();

    let mut creation_requests = Vec::new();
    let mut update_requests = Vec::new();

    for id in ids {
        match (ical_events.get(&id), notion_events.get(&id)) {
            (Some(ical_event), Some(notion_event)) => {
                if let Some(query) = sync.update_request(ical_event, notion_event) {
                    update_requests.push((
                        ical_event.get_summary().unwrap_or_default(),
                        &notion_event.id,
                        query,
                    ));
                }
            }
            (Some(ical_event), None) => {
                if let Some(date) = ical_event.get_start() {
                    let start = match date {
                        DatePerhapsTime::DateTime(dt) => dt
                            .try_into_utc()
                            .context("Calendar without time zone")?
                            .date_naive(),
                        DatePerhapsTime::Date(dt) => dt,
                    };
                    if start < earliest || start > latest {
                        continue;
                    }
                }
                creation_requests.push((
                    ical_event.get_summary().unwrap_or_default(),
                    sync.create_request(ical_event),
                ));
            }
            (None, Some(_notion_event)) => {
                debug!("Event {} is in Notion but not in ICS", id);
            }
            (None, None) => {
                unreachable!()
            }
        }
    }

    info!(
        "Creating {} events and updating {} events",
        creation_requests.len(),
        update_requests.len()
    );

    for (title, request) in creation_requests {
        info!("Creating event {}", title);
        if !dry_run {
            client.create_page(request).await?;
        }
    }

    for (title, page, request) in update_requests {
        info!("Updating event {}", title);
        if !dry_run {
            client.update_page(page, request).await?;
        }
    }

    info!("Synchronization successful");

    Ok(())
}
