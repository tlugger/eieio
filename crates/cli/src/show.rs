//! `eio service show` — the graph, with names resolved (SERVICE-SPEC §9.1).
//!
//! SERVICE §5 keeps connections id-only so that a name is never load-bearing, and accepts in
//! exchange that raw TOML makes a human cross-reference the block tables by hand. This is the
//! tooling that pays that back, and it is the whole reason §5 could make that trade.
//!
//! It renders and never writes, so nothing here has to worry about preservation.

use std::fmt::Write as _;

use eio_service::Parsed;

/// The service, as a person reads it.
pub fn render(parsed: &Parsed) -> String {
    let service = &parsed.service;
    let mut out = String::new();

    let _ = writeln!(
        out,
        "{}{}",
        service.name,
        if service.autostart {
            "  (autostart)"
        } else {
            ""
        }
    );

    if !service.blocks.is_empty() {
        let _ = writeln!(out, "\nblocks");
        // Rendered once, then measured and printed from the same strings — the same shape the
        // connections section below uses, and the reason neither renders a label twice.
        let rows: Vec<(&String, String, &eio_service::Instance)> = service
            .blocks
            .iter()
            .map(|(id, instance)| (id, label(instance.name.as_deref()), instance))
            .collect();
        // One column width for the whole table, so ids and labels line up down the page and
        // the block references start in the same place.
        let id_width = rows.iter().map(|(id, ..)| id.len()).max().unwrap_or(0);
        let label_width = rows
            .iter()
            .map(|(_, label, _)| label.len())
            .max()
            .unwrap_or(0);

        for (id, label, instance) in &rows {
            let _ = writeln!(
                out,
                "  {id:id_width$}  {label:label_width$}  {}",
                instance.block
            );
            let property_width = instance.props.keys().map(String::len).max().unwrap_or(0);
            for (property, expression) in &instance.props {
                let _ = writeln!(out, "      {property:property_width$} = {expression}");
            }
        }
    }

    if !parsed.connections.is_empty() {
        let _ = writeln!(out, "\nconnections");
        // Both ends rendered the same way and to one width, so the arrows line up: an edge
        // list a reader scans down is the thing that makes the id-only format legible.
        let terminals: Vec<(String, String)> = parsed
            .connections
            .iter()
            .map(|connection| {
                (
                    terminal(parsed, &connection.from.instance, &connection.from.port),
                    terminal(parsed, &connection.to.instance, &connection.to.port),
                )
            })
            .collect();
        let from_width = terminals
            .iter()
            .map(|(from, _)| from.len())
            .max()
            .unwrap_or(0);

        for (from, to) in &terminals {
            let _ = writeln!(out, "  {from:from_width$}  ->  {to}");
        }
    }

    out
}

/// `id "Label" .port`, with the label resolved from the block table.
///
/// An instance a connection names that the file does not define cannot reach here — that is
/// SERVICE §7's dangling-connection error and [`eio_service::parse`] has already refused it.
fn terminal(parsed: &Parsed, id: &str, port: &str) -> String {
    let name = parsed
        .service
        .instance(id)
        .and_then(|instance| instance.name.as_deref());
    format!("{id} {} .{port}", label(name))
}

/// A label, or a placeholder for the instances that have none.
///
/// `name` is OPTIONAL (SERVICE §2) and most generated instances will not carry one, so the
/// placeholder keeps the columns honest rather than collapsing them.
fn label(name: Option<&str>) -> String {
    match name {
        Some(name) => format!("{name:?}"),
        None => String::from("-"),
    }
}
