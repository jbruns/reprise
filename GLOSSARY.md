# Title Select

Title Select audits Source Sets against DAT Revisions and, later, builds explained Selections from them into a Play
Library. "Title Select" is the product's name, not a domain term.

## Inventory

**DAT**:
A published inventory of expected game files and their identifying checksums (e.g. No-Intro, Redump, MAME).
_Avoid_: datfile, database

**DAT Revision**:
One particular edition of a DAT, identified by the SHA-256 of its unmodified file. A newer revision does not replace
the reference used by an earlier Audit or Selection.

**Game**:
An entry in a DAT Revision: one release, made of one or more ROMs. The product says Game; Title is an accepted
synonym in conversation.
_Avoid_: set

**ROM**:
One expected file of a Game, with its name, size and checksums as the DAT Revision gives them. Also used for any
file, such as a `.wav` or `.cue`, that a DAT lists.

## Collections

**Source Set**:
A preserved collection, kept with its files, variants and provenance, that can be audited against its DAT and is
recorded with the DAT Revision of its latest Audit; the source from which a Play Library is built. It changes only
through an approved repair Change Plan.
_Avoid_: Verified Set, Archive, romset, dump folder

**Play Library**:
The selected collection that players and front ends read, derived for a device, purpose or preference. Title Select
builds Selections into it.
_Avoid_: Library (alone), output

**Ingest Folder**:
A folder where new files arrive, such as a Downloader's completed folder or a manual drop, that the Operator registers
for Title Select to assess. Title Select never writes to it.
_Avoid_: Source (alone), Downloads

**Root**:
A named folder the Operator configures for Title Select to read, or later to write, such as the one holding Source
Sets. A Source Set or a Play Library folder is identified by its Root and a path relative to it.
_Avoid_: Mount, share

**Operator**:
The person who runs Title Select and approves its Change Plans.

**Job**:
A durable unit of work that Title Select runs for the Operator, such as an Audit. It is Queued, Running, Succeeded,
Failed or Interrupted, and it survives a browser disconnecting or the Service restarting.
_Avoid_: Task, run

**Operator Addition**:
An item the Operator approves for management in a Play Library independently of an official DAT, such as a ROM hack. Its
recorded provenance and local integrity checks do not make it an official DAT match.

## Audit

**Audit**:
An assessment of a Source Set against a particular DAT Revision, recording discrepancies and the verification
evidence actually obtained.

**Entry**:
One file an Audit finds in a Source Set: a loose file, or a member of a ZIP archive.

**Evidence**:
What an Audit actually checked for an Entry: the Evidence Level and the checksum algorithms compared.

**Evidence Level**:
How an Audit knows an Entry's checksums. _Claim_: taken from an archive's directory without reading the payload.
_Payload_: computed by reading every byte.

**Finding**:
An Audit's verdict on one Entry:

- _Matched_: its payload matches the ROM its name gives.
- _Misnamed_: its payload matches a ROM, and its name is not a ROM in the DAT Revision.
- _Conflicting_: its name is one ROM, but its payload matches another.
- _Mismatched_: its name is a ROM, but its payload matches no ROM. This is not proof of damage.
- _Unmatched_: neither its name nor its payload matches a ROM.
- _Unreadable_: it could not be read; the reason is recorded. Never reported as any other Finding.

_Avoid_: Bad, Extra

**Verified**:
An Entry whose Finding is Matched with Payload Evidence. Claim Evidence never makes an Entry Verified.

**Needs Review**:
An Entry whose Finding is Misnamed, Conflicting, Mismatched, Unmatched or Unreadable, or a Selection decision that
remains unresolved.

**Game Status**:
An Audit's verdict on one Game: _Complete_ (every ROM present), _Incomplete_ (some), or _Missing_ (none). A
Misnamed or Conflicting Entry counts towards the Game its payload matches.

## Selection and change

**Selection Profile**:
The Operator's rules for including or excluding releases and choosing preferred releases of a Game: global settings
with per-system settings. It holds no DAT content and is reusable across DATs. Each saved version is immutable.
Profile is its short form.
_Avoid_: Filtered DAT (when meaning the rules)

**Selection**:
The exact releases chosen from a particular DAT Revision using a Selection Profile and any explicit Operator
overrides, each with the rule responsible.
_Avoid_: Profile (when meaning the chosen releases), Filtered DAT

**Change Plan**:
A proposed set of changes that the Operator approves before a Source Set is repaired or a Selection is built into a
Play Library. Approval to repair a Source Set is separate from approval to build a Play Library.

**Build**:
Applying an approved Change Plan that creates or updates a Play Library from a Selection's files in a Source Set.
_Avoid_: Publish, export
