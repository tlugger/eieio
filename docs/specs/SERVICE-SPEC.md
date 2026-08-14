# Service File Specification

**Status:** Draft 1 — normative. **Depends on:** ABI-SPEC.md §5.2, §6.4, §11 (ports, properties, the error port), EXPR-SPEC.md §10 (static analysis), DAEMON-SPEC.md §2–§3 (on-disk layout, boot), SCOPE.md §3.6, §3.8. **Markers:** **PROPOSED** = drafted here, awaiting ratification. **OPEN** = tracked in SCOPE.md.

A **service** is a graph of block instances on one node, and it is one file. This document specifies that file: what it says, what it means, and what a host MUST refuse.

The key words MUST, MUST NOT, SHOULD, and MAY are used as in RFC 2119.

It is separate from DAEMON-SPEC because its readers are: the daemon that runs a service, the Designer that edits one (DESIGNER §4), the CLI that scaffolds one, and whatever an agent writes. Only the first is the daemon's.

---

## 1. The file

TOML. One file, one service, and the service is the deployable unit (DAEMON §2). It lives at `<data-dir>/services/<name>.toml`, and **the stem MUST equal the service's `name`** — `kitchen.toml` declares `name = "kitchen"`. §3 requires a name to be unique per node and this is what makes that structural rather than checked: a filesystem has already refused the second `kitchen.toml`. It is also what lets the management API address a service by name without holding an index from names to filenames, which would be state the API kept that the files did not (DAEMON §2). A host MUST refuse a file whose stem and `name` disagree; §7's "one service failing MUST NOT stop another" covers it like any other invalidity.

```toml
name = "kitchen"
autostart = true

# Every top-level key comes before the first table header — see §5.
connections = [
  "b7k2.out -> f3m9.in",
  "f3m9.above -> k1p8.in",
  "f3m9.err -> k1p8.in",
]

[blocks.b7k2]
name = "Thermometer"
block = "ghcr.io/tlugger/temp-sensor:1.0.0"

[blocks.b7k2.props]
interval_ms = "5000"

[blocks.f3m9]
name = "Too cold?"
block = "filter:1.2.0"

[blocks.f3m9.props]
reading = "(float $temp)"
threshold = "18.0"

[blocks.k1p8]
name = "Alarm"
block = "publisher:1.0.0"

[blocks.k1p8.props]
topic = "\"kitchen.cold\""

[ui]
# Anything. The daemon parses this table and never reads inside it (§6).
viewport = { x = 0, y = 0, zoom = 1.0 }

[ui.blocks.b7k2]
x = 148
y = 234
```

## 2. Identity: the id, and why it is not the name

**A block instance is identified by its id, which is the table key.** `name` is a label for people and agents, and carries no meaning to a host.

This is the one structural decision the rest of the format follows from, and it is taken from what the predecessor got right. nio identified a configured block by a generated UUID and carried `name` beside it, so a connection referred to `54b735e8-…` and renaming a block touched exactly one field. The alternative — the name *is* the identity — makes every connection, every layout annotation and every API path a second place the name is written down, so renaming a block is a refactor and two blocks may never share a label.

Concretely:

- **The key is the id.** There is no `id` field. A service file therefore cannot have an instance without one, and cannot have two instances with the same one — TOML rejects a duplicate key before this specification has to.
- **`name` is OPTIONAL and MAY be repeated.** Two instances of the same block doing the same job to two sensors may both be called `"Thermometer"`. A host MUST NOT resolve anything by name.
- **Connections, `[ui]` entries and API paths address the id** and nothing else.

**Who mints an id.** Tooling, at authoring time: the Designer when a block is dropped on the canvas, the CLI when a block is added to a file. **A host MUST NOT write to a service file** — not to add an id, not to normalize one. Editing a file by hand and calling reload is a first-class path (SCOPE §3.8), and a reload that rewrote the file would make `git status` dirty after every deploy and would put a formatting policy where a human's formatting was.

**An id is opaque.** A generated one SHOULD be short and unmemorable — this specification's reference generator emits four characters — but a host MUST NOT require that it look generated. `[blocks.thermo]` is a valid instance whose id is `thermo`. What the format asks of an id is that it be *stable*, because connections point at it; whoever chooses a meaningful id has chosen to rename it never, and that is their business.

### 2.1 Id syntax (normative)

|What|Pattern|Bound|
|---|---|---|
|A block instance id|`^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$`|≤64 bytes|

**This is ABI §11.1's port-and-property rule, not a rule that resembles it.** The two reasons are the same — both are TOML bare keys, and both exclude `.` because `id.port` is how a connection addresses a terminal — so an implementation SHOULD share the constant rather than restate the pattern, and this repository's does. The consequence is intended: an id and a port name cannot drift apart, which is what keeps a connection parseable by construction. A single-character id is legal; `-` and `_` may not lead or trail.

Ids are unique within a service file and mean nothing outside it. Two services on one node may both contain `b7k2`, and they are not related.

## 3. Top level

|Field|Type|Required|Meaning|
|---|---|---|---|
|`name`|string|REQUIRED|The service's name. Unique per node; the API addresses a service by it|
|`autostart`|bool|OPTIONAL, default `false`|Whether the daemon starts this service at boot (DAEMON §3)|
|`blocks`|table|OPTIONAL, default empty|Block instances, keyed by id (§4)|
|`connections`|array of strings|OPTIONAL, default empty|The wiring (§5)|
|`ui`|table|OPTIONAL|Designer annotations the daemon MUST NOT interpret (§6)|

`name` follows ABI §11.1's port-and-property pattern — `^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$`, ≤64 bytes — because a service name is a path component in the management API and a filename on disk.

**Unknown fields MUST be rejected**, at the top level and within every nested table except `[ui]` and `[blocks.<id>.props]`. The reason ABI §11.1 gives applies unchanged: a typo'd `autostrat = true` that silently meant nothing is the failure this prevents. The two exceptions are the two places whose keys are *data* — a property name is the block's to choose, and `[ui]` is not the daemon's to read at all.

A service with no blocks is valid. It runs nothing, which is what it says.

## 4. Block instances

```toml
[blocks.<id>]
name  = "..."       # OPTIONAL. A label; no meaning to a host (§2)
block = "..."       # REQUIRED. The block reference (SCOPE §3.6)

[blocks.<id>.props]
<property> = "<expression>"
```

`block` is a registry reference: what the block manager resolves, pulls and digest-verifies (DAEMON §4). Its grammar is the registry's, not this format's, and this specification does not constrain it beyond requiring a non-empty string — a service file that named a block by a syntax the node's registry does not accept fails at resolution, with the registry's error, which says more than a pattern here could.

**Every property value is an expression string** (ABI §11): there is no static/dynamic split, and a literal is a trivial expression. `threshold = "18.0"` and `reading = "(float $temp)"` are the same kind of thing. A string property's value is therefore a *quoted* expression — `topic = "\"kitchen.cold\""` — which is the one piece of friction the model costs and is the price of one rule instead of two.

Every value in `props` MUST be a string. A TOML number or boolean is not an expression, and accepting one would be inventing the second kind of property ABI §11 exists to refuse. The error names the property and says so.

A property the block does not declare is an error (§7). A declared property absent here takes its manifest `default`, and configuration fails if it is `required` with neither (ABI §11.1).

## 5. Connections

```toml
connections = [ "<id>.<port> -> <id>.<port>" ]
```

One string per edge. The grammar, exactly:

```
connection  = source ws* "->" ws* destination
source      = id "." port
destination = id "." port
```

Whitespace around the arrow is OPTIONAL and any amount; whitespace elsewhere is an error rather than trimmed, because `"b7k2 .out"` is a typo and reading it as `b7k2.out` teaches that the format guesses.

An edge means: signals emitted on `source`'s output port are delivered to `destination`'s input port. Fan-out is several edges from one source; fan-in is several into one destination. Both are ordinary (DAEMON §5).

**The error port.** ABI §6.4 gives every block an output port named `err` that appears in no manifest. It is addressable here as a source — `"f3m9.err -> k1p8.in"` — and MUST NOT appear as a destination, because it is an output. An unrouted `err` emission is logged and counted, so leaving it unconnected is a choice and not an omission.

**Duplicate edges are an error.** The same source and destination twice would deliver each batch twice, which no one means; a fan-out to the *same* input from two different outputs is not a duplicate and is fine.

**A self-edge — `"b7k2.out -> b7k2.in"` — is legal.** A block that feeds itself is a legitimate topology (an accumulator, a retry loop), and the ABI makes it safe by construction: `emit` enqueues and routing happens after the callback returns (ABI §6.2), so a self-edge cannot re-enter the guest. What it can do is fill the instance's own mailbox, which DAEMON §5 already answers.

**Where the array sits in the file matters, and TOML is why.** A top-level key after a table header belongs to *that table*: `connections` written below `[blocks.b7k2]` is a key of `b7k2`, not of the service. So every top-level field — `name`, `autostart`, `connections` — MUST appear before the first `[blocks.…]` or `[ui]` header. This is TOML's rule rather than this format's, and it is stated here because appending a connection to the end of a file is the obvious thing to do and is wrong.

It is at least never *silently* wrong. §3 rejects unknown fields, so a `connections` that landed in a block table is an unknown field of that table; and if it landed in a `props` table instead, §4 requires every property value to be a string, which an array is not. Both say so at parse time.

Ids only. There is no name resolution here, deliberately: the moment a connection may name a block, a name is load-bearing again and §2's whole argument is undone. Rendering a graph for a human is tooling's job, from the same file.

## 6. `[ui]`

The Designer's annotations: canvas positions, viewport, notes (DESIGNER §4). Layout lives in the service file so that the file stays the single portable artifact — clone a service onto a fresh node and it renders laid out.

**The daemon MUST parse `[ui]` and MUST NOT interpret it.** It has no schema here and never will; a daemon that read a key inside it would make the Designer's layout format a thing the daemon has an opinion about. It MUST survive a read-modify-write unchanged, which is what makes it safe to put a human's canvas in a file a program rewrites.

Its keys are conventionally block ids, and that is a convention of the Designer's rather than a rule of this format. An entry naming an id the file does not define is **not** an error: a block was deleted and a stale annotation is inert.

## 7. Validation

A service file that violates any rule above is invalid. Validation happens in two stages, because they need different things.

**Stage 1 — self-contained.** Everything checkable from the file alone: TOML syntax, unknown fields, the id and name patterns, connection grammar, connections naming an instance the file does not define, duplicate edges, `err` as a destination, non-string property values, and every property expression parsing and passing EXPR §10's static analysis. A file that fails here is wrong on its face, and no registry needs to be reachable to say so.

**Stage 2 — manifest-dependent.** What needs the blocks resolved (DAEMON §4): that a connection's ports exist on the manifests of the instances it names, in the right direction, and that every configured property is one the block declares. This stage takes resolved manifests as an input rather than fetching them, so the same function serves the daemon at boot, the Designer against its cache, and a CLI against a local build.

Each of the following is a **distinct** error, carrying enough to point at the offending text:

|Class|Example|
|---|---|
|Malformed TOML|a missing bracket|
|Unknown field|`autostrat = true`|
|Empty block reference|`block = ""`|
|Bad id, service name|`[blocks.Thermo]`|
|Bad connection syntax|`"b7k2.out => f3m9.in"`|
|Dangling connection|`"b7k2.out -> nope.in"`|
|Duplicate connection|the same edge twice|
|`err` as a destination|`"a.out -> b.err"`|
|Non-string property|`threshold = 18.0`|
|Unparsable expression|`threshold = "(+ 1"`|
|Expression rejected by static analysis|`threshold = "(nosuchfn 1)"`|
|Unknown port (stage 2)|`"b7k2.nope -> f3m9.in"`|
|Unknown property (stage 2)|a property the manifest does not declare|

"Distinct" means a caller can tell them apart without matching on a message. The Designer renders a validation failure on the offending block, property or connection (DESIGNER §5), which it cannot do from a string. The last two rows are distinguished by EXPR §8's error *code*, which an implementation MUST carry rather than render: "does not parse" and "cannot mean anything" call for different fixes.

**One exception, and it is a limit rather than a choice.** Malformed TOML and an unknown field are both whatever the TOML parser returned, and a parser is not obliged to say structurally which it was. An implementation MAY report them as one class carrying that parser's message, which already names the line and the key. Splitting them by matching on that message would be the thing this rule exists to spare a caller.

**One service failing MUST NOT stop another**, or the daemon (DAEMON §3): the failed service surfaces as errored through the API.

## 8. What this format does not do

- **It does not name a node.** A service file is deployed *to* a node; which one is the deployer's business and belongs to no line in the file. Cross-node connections are the pub/sub epic's, through `publisher`/`subscriber` blocks that are ordinary instances here (DAEMON §6).
- **It does not carry secrets.** A property is an expression, and an expression is public. Credentials reach a block through node configuration (DAEMON §2, SCOPE §3.11 OPEN).
- **It does not version itself.** The file has no schema version field: an additive change is compatible by §3's rules, and a breaking one would be a new format with a new name. A version field invites a daemon to support two.

## 9. Editing a service file

§2 says a host MUST NOT write to a service file, and this section is the other half of that sentence: what the tooling that *does* write one owes the person whose file it is.

**A structural edit is a read-modify-write that preserves everything it did not change.** Comments, key order, alignment, blank lines, inline-versus-multi-line arrays and quoting style all survive; the diff of a file before and after an edit MUST show the edit and nothing else. This is not a nicety. §2's whole argument for hand-editing being first class collapses if the first Designer drag reformats a file a human wrote, and DESIGNER §4 makes it a hard requirement for exactly that reason. A value-tree parser cannot do it — a value tree has no trivia — so an implementation needs a preserving parser, and this repository's editor is `eio-service`'s (`toml_edit` underneath).

**`[ui]` survives unchanged.** §6 already requires it; it is restated here because a read-modify-write is the operation that would break it, and because the Designer is the one caller that writes inside it.

Four rules an editor MUST follow, each of them a way the format can be violated by a well-meaning write:

- **A top-level key stays above the first table header.** §5's rule is a fact about TOML, and an editor that appended `connections` to the end of a file would file it under the last block. An implementation MAY satisfy this by construction — a preserving parser that renders root key-values before sub-tables does — but it MUST NOT emit a file where a top-level key sits below a header.
- **Removing a block removes the connections that name it.** A connection naming an instance the file does not define is §7's dangling-connection error, so the alternative to cascading is writing a file that will not load. What is removed is the block's business to report; that it happens is not optional.
- **Removing a block does not touch `[ui]`.** §6 makes a stale annotation inert, and an editor that tidied it would be deciding that `[ui]`'s keys are block ids — a schema §6 says this format does not have.
- **An edit that would make the file invalid MUST fail and change nothing.** The file on disk and the document in hand are both left as they were, and the caller is told which rule it broke. Writing a file that §7 rejects and calling it the caller's problem is how a service ends up errored by its own tooling.

An editor SHOULD re-run §7 stage 1 over what it is about to write. Stage 1 needs nothing but the text, so there is no reason not to, and it is what makes the preceding rule provable rather than asserted.

**Who edits.** The `eio service` CLI (§9.1), the Designer through its backend, and an agent through either. Not the daemon: DAEMON §9.3's `PUT` stores the bytes a client composed and edits none of them, which is what keeps §2's rule true of a node while this section is true of everything else. The conflict detection that makes concurrent editing safe is that endpoint's, and is specified there.

### 9.1 `eio service`

The command surface for authoring a service file, in the `eio` binary (SCOPE §5.1, DAEMON §1). It is a **local** tool: every command reads and writes a file, none of them contacts a node, and nothing here needs a daemon to be running. Deploying what it produced is `PUT` or a git push, and both are somebody else's section.

```
eio service new           <name> [--dir D] [--autostart]
eio service add-block     <file> --block REF [--name LABEL] [--id ID] [--prop K=EXPR]…
eio service remove-block  <file> <id>
eio service connect       <file> <id.port> <id.port>
eio service disconnect    <file> <id.port> <id.port>
eio service set-prop      <file> <id> <property> <expression>
eio service unset-prop    <file> <id> <property>
eio service set-autostart <file> <true|false>
eio service show          <file>
eio service validate      <file> [--manifest REF=PATH]…
```

**`add-block` mints the id**, and that is the command's reason to exist. §2 puts id-minting on tooling at authoring time, and until there was a command that added a block to a file, the rule described nobody. A generated id is checked against the ids the file already uses and is **printed**, because the next thing its author does is write a connection naming it. `--id` supplies one instead, held to §2.1 like any other.

**Every mutating command is a §9 edit**: it reads the file, applies exactly what it was asked, re-runs §7 stage 1, and only then writes. A command that would leave the file invalid changes nothing and says which rule it broke. The write is atomic — the file is replaced, never truncated in place — because it is a file a person has open in an editor and a half-written service file is worse than a failed command.

**Nothing is announced until it is on disk.** The refusal can arrive *after* the edit was applied in memory, because stage 1 runs last — so a command that reported as it went would describe an edit that does not exist. `add-block` is the case that makes this load-bearing rather than tidy: it prints a minted id precisely so its author can wire it up next, and an id printed for a block that was never written sends them to a `connect` that cannot work. A failed command writes nothing to standard output, which is what lets a caller that reads only standard output — an agent, per SCOPE §4 — believe it.

**`show` resolves names, and that is the whole of what it is for.** §5 keeps connections id-only so that a name is never load-bearing, and accepts in exchange that raw TOML makes a human cross-reference the block tables by hand. This is the tooling that pays that back: instances with their labels, then every edge with both ends' labels beside their ids. It renders and never writes.

**`validate` runs both stages and distinguishes every class.** Stage 1 needs only the file. Stage 2 needs manifests, and §7 makes them an *input* rather than something the stage fetches — so they are supplied as `--manifest REF=PATH`, keyed by the `block` reference the file already writes, which is what lets two instances of one block share one manifest.

A block whose manifest was not supplied is reported as **not checked**, and **stage 2's own result line says how much of it ran** — every block, some of them, or none. Both halves are needed for the same reason: a partial stage 2 must not read as a complete one, and a caller scanning for the result line reads *that* rather than the notices under it. A service with no blocks has nothing to check, which is a third answer again and not a skip.

§1's stem-equals-name rule is checked here too, and refusing it is the point: a file the CLI accepted and a node refuses would make this command worth less than reading the specification.

## 10. Expansion list (for the in-depth pass)

Whether a service may reference another service's ports.
