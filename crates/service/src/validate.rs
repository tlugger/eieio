//! SERVICE-SPEC §7's stage 2: what needs the blocks resolved.

use eio_manifest::Manifest;

use crate::error::ResolvedError;
use crate::parse::Parsed;

/// Checks a parsed service against the manifests of the blocks it names.
///
/// Takes the manifests rather than fetching them, which is the seam that lets one function
/// serve three callers: the daemon at boot with what the block manager pulled (DAEMON §4),
/// the Designer against its cache, and a CLI against a block built locally. Resolution is
/// nobody's business here — this crate knows what a service file *means*, not where blocks
/// come from.
///
/// `manifest` answers for an instance id. An id it cannot answer for is **skipped, not an
/// error**: a service whose blocks are not all resolvable yet is a stage-2 run the caller
/// chose to make, and reporting "unknown port" for a block nobody could look up would blame
/// the file for the registry being unreachable.
pub fn validate(
    parsed: &Parsed,
    manifest: impl Fn(&str) -> Option<Manifest>,
) -> Vec<ResolvedError> {
    let mut errors = Vec::new();

    for (index, connection) in parsed.connections.iter().enumerate() {
        // A source is an output and a destination is an input. The error port is the one
        // exception: ABI §6.4 gives it to every block without declaring it, so it resolves
        // against no manifest and is legal wherever §5 permits it — which the parse stage
        // has already decided.
        if connection.from.port != eio_manifest::PORT_ERR_NAME
            && let Some(block) = manifest(&connection.from.instance)
            && block.output_index(&connection.from.port).is_none()
        {
            errors.push(ResolvedError::UnknownPort {
                index,
                instance: connection.from.instance.clone(),
                port: connection.from.port.clone(),
                direction: "output",
            });
        }

        if let Some(block) = manifest(&connection.to.instance)
            && block.input_index(&connection.to.port).is_none()
        {
            errors.push(ResolvedError::UnknownPort {
                index,
                instance: connection.to.instance.clone(),
                port: connection.to.port.clone(),
                direction: "input",
            });
        }
    }

    for (id, instance) in &parsed.service.blocks {
        let Some(block) = manifest(id) else {
            continue;
        };
        for property in instance.props.keys() {
            if block.prop_id(property).is_none() {
                errors.push(ResolvedError::UnknownProperty {
                    id: id.clone(),
                    property: property.clone(),
                });
            }
        }
    }

    errors
}
