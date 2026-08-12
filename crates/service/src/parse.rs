//! Reading a service file, and SERVICE-SPEC §7's stage 1.

use std::collections::BTreeMap;

use crate::connection::Connection;
use crate::error::Error;
use crate::id;
use crate::schema::Service;

/// A service file, parsed and checked as far as the file alone allows (SERVICE §7 stage 1).
///
/// Holds the connections already parsed, because every later stage wants them that way and
/// re-parsing a string that has already been validated is a second chance to disagree with
/// the first reading.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    /// The document.
    pub service: Service,
    /// Its connections, in file order, each parsed (SERVICE §5).
    pub connections: Vec<Connection>,
}

/// Parses and runs stage 1.
///
/// Every error the file can carry on its own face, collected rather than returned one at a
/// time: an operator fixing a service file should see the whole list, and a Designer renders
/// all of them at once (DESIGNER §5).
///
/// A `Vec` that is empty means valid; the caller then runs [`crate::validate`] with resolved
/// manifests for stage 2.
pub fn parse(text: &str) -> Result<Parsed, Vec<Error>> {
    // TOML first, and alone: nothing else can be said about a document that is not one.
    let service: Service = match toml::from_str(text) {
        Ok(service) => service,
        Err(error) => return Err(vec![Error::Toml(error.to_string())]),
    };

    let mut errors = Vec::new();

    // The service's own name is a path component in the API and a filename on disk
    // (SERVICE §3), so it is held to the same pattern as everything else that is both.
    if !id::is_id(&service.name) {
        errors.push(Error::ServiceName {
            name: service.name.clone(),
        });
    }

    for (instance_id, instance) in &service.blocks {
        if !id::is_id(instance_id) {
            errors.push(Error::InstanceId {
                id: instance_id.clone(),
            });
        }
        // The reference's *grammar* is the registry's, not this format's (SERVICE §4). All
        // that can be said here is that there is one.
        if instance.block.trim().is_empty() {
            errors.push(Error::EmptyBlockRef {
                id: instance_id.clone(),
            });
        }
        check_properties(instance_id, &instance.props, &mut errors);
    }

    let connections = check_connections(&service, &mut errors);

    if errors.is_empty() {
        Ok(Parsed {
            service,
            connections,
        })
    } else {
        Err(errors)
    }
}

/// Every property expression parses and passes EXPR §10's static analysis.
///
/// The real front end, not an approximation of it: a service file that validates here must
/// configure on a node, and the only way to promise that is to ask the same code.
fn check_properties(instance_id: &str, props: &BTreeMap<String, String>, errors: &mut Vec<Error>) {
    for (property, source) in props {
        // A parse failure and an analysis diagnostic are the same shape — EXPR §8's code,
        // span and message — so they are carried the same way. What tells them apart is the
        // code, which is exactly what SERVICE §7 asks a caller to be able to do.
        let diagnostics = match eio_expr::analyze_source(source) {
            Err(error) => vec![error],
            // Every diagnostic, not the first: an editor shows them all at once, and an
            // expression with two unbound symbols has two things to fix.
            Ok(analysis) => analysis.diagnostics,
        };
        for diagnostic in diagnostics {
            errors.push(Error::Property {
                id: instance_id.to_string(),
                property: property.clone(),
                code: diagnostic.code,
                span: diagnostic.span,
                message: diagnostic.message,
            });
        }
    }
}

/// Connection syntax, dangling ids, duplicate edges, and `err` as a destination.
///
/// Returns what parsed. An entry that failed contributes an error and no connection, so the
/// result is shorter than the input exactly when the input was wrong — and the caller only
/// ever sees it on the `Ok` path, where nothing failed.
fn check_connections(service: &Service, errors: &mut Vec<Error>) -> Vec<Connection> {
    let mut parsed: Vec<Connection> = Vec::with_capacity(service.connections.len());
    // Index of the first entry that said each thing, so a duplicate can name its original.
    let mut seen: BTreeMap<(String, String, String, String), usize> = BTreeMap::new();

    for (index, text) in service.connections.iter().enumerate() {
        let connection = match Connection::parse(text) {
            Ok(connection) => connection,
            Err(error) => {
                errors.push(Error::ConnectionSyntax {
                    index,
                    text: text.clone(),
                    error,
                });
                continue;
            }
        };

        // ABI §6.4: `err` is an output every block has and no block declares, so it is a
        // legitimate source and never a destination.
        if connection.to.port == eio_manifest::PORT_ERR_NAME {
            errors.push(Error::ErrorPortDestination { index });
        }

        for (terminal, side) in [
            (&connection.from, "source"),
            (&connection.to, "destination"),
        ] {
            if service.instance(&terminal.instance).is_none() {
                errors.push(Error::DanglingConnection {
                    index,
                    instance: terminal.instance.clone(),
                    side,
                });
            }
        }

        let key = (
            connection.from.instance.clone(),
            connection.from.port.clone(),
            connection.to.instance.clone(),
            connection.to.port.clone(),
        );
        match seen.get(&key) {
            Some(first) => errors.push(Error::DuplicateConnection {
                index,
                first: *first,
            }),
            None => {
                seen.insert(key, index);
            }
        }

        parsed.push(connection);
    }
    parsed
}
