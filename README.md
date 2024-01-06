# Notion-ics

A one way synchronization from a public ICS calendar to a Notion database.

## Motivation

N/A

## Limitations

Many

## Settings

```toml
ical_url = "https://example.com/calendar.ics"
day_past = 60
day_future = 60
notion_token = "secret_token"
notion_calendar = "Calendar database name"
id_property = "String property name managed by notion-ics"
date_property = "Date property"
location_property = "Location property (optional)"
```

## Cli

Run once: `notion_ics --config settings.toml`

Dry run: `notion_ics --config settings.toml --dry-run`

Scheduled run every day: `notion_ics --config settings.toml --schedule "@daily"`
